use super::MdbaseRecordCacheError;
use crate::mdbase::control_access::ControlAccess;
use crate::mdbase::{
    bundled_mdbase_schema, collect_external_schema_references, parse_local_schema,
    schema_reference_path, MDBASE_SCHEMA_MAX_BYTES, MDBASE_SCHEMA_MAX_DEPTH,
    MDBASE_SCHEMA_MAX_FILES,
};
use crate::paths::secure_open_regular_read;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

pub(super) type SchemaSources = BTreeMap<PathBuf, Option<Vec<u8>>>;

/// Discover file dependencies without compiling schemas or reading records.
/// Invalid and absent references retain their observed bytes/absence, so repair
/// invalidates the revision too. Invalid remote/escaping references are never read.
pub(super) fn schema_sources(
    root: &Path,
    controls: impl Iterator<Item = (PathBuf, String)>,
    access: &ControlAccess<'_>,
) -> Result<SchemaSources, MdbaseRecordCacheError> {
    let mut sources = BTreeMap::new();
    for (path, source) in controls {
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let parsed = crate::parser::parse_document(&source, &crate::config::VaultConfig::default());
        let Some(frontmatter) = parsed.raw_frontmatter else {
            continue;
        };
        let Ok(frontmatter) = serde_yaml::from_str::<serde_yaml::Value>(&frontmatter) else {
            continue;
        };
        let Ok(frontmatter) = serde_json::to_value(frontmatter) else {
            continue;
        };
        for key in [
            "schema",
            "record_schema",
            "binding_schema",
            "data_schema",
            "source_schema",
            "input_schema",
            "output_schema",
            "error_schema",
            "provider_schema",
        ] {
            let Some(wrapper) = frontmatter.get(key) else {
                continue;
            };
            let mut walker = Walker {
                root,
                access,
                sources: &mut sources,
                visited: BTreeSet::new(),
            };
            if let Some(value) = wrapper.get("value") {
                walker.references(value, &path, 0)?;
            } else if let Some(reference) = wrapper.get("ref").and_then(serde_json::Value::as_str) {
                walker.reference(reference, &path, 0, true)?;
            }
        }
    }
    Ok(sources)
}

struct Walker<'a> {
    root: &'a Path,
    access: &'a ControlAccess<'a>,
    sources: &'a mut SchemaSources,
    visited: BTreeSet<PathBuf>,
}

impl Walker<'_> {
    fn references(
        &mut self,
        schema: &serde_json::Value,
        base: &Path,
        depth: usize,
    ) -> Result<(), MdbaseRecordCacheError> {
        let mut references = Vec::new();
        collect_external_schema_references(schema, &mut references);
        references.sort_unstable();
        references.dedup();
        for reference in references {
            self.reference(reference, base, depth, false)?;
        }
        Ok(())
    }

    fn reference(
        &mut self,
        reference: &str,
        base: &Path,
        depth: usize,
        select_fragment: bool,
    ) -> Result<(), MdbaseRecordCacheError> {
        let (file, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        if file.is_empty()
            || bundled_mdbase_schema(file).is_some()
            || file.contains("://")
            || file.starts_with("urn:")
            || file.contains('?')
        {
            return Ok(());
        }
        let Ok(path) = schema_reference_path(
            self.root,
            base.parent().unwrap_or(Path::new("")),
            Path::new(file),
        ) else {
            return Ok(());
        };
        if !self
            .access
            .path_allowed(&path.to_string_lossy().replace('\\', "/"))
        {
            return Err(MdbaseRecordCacheError::PermissionDenied);
        }
        if !self.visited.insert(path.clone()) {
            return Ok(());
        }
        if depth > MDBASE_SCHEMA_MAX_DEPTH || self.visited.len() > MDBASE_SCHEMA_MAX_FILES {
            return Err(read_error(
                self.root,
                &path,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "schema dependency traversal limit exceeded",
                ),
            ));
        }
        if !self.sources.contains_key(&path) {
            let contents = match secure_open_regular_read(self.root, &path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take(MDBASE_SCHEMA_MAX_BYTES + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|error| read_error(self.root, &path, error))?;
                    if bytes.len() as u64 > MDBASE_SCHEMA_MAX_BYTES {
                        return Err(read_error(
                            self.root,
                            &path,
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "schema dependency byte limit exceeded",
                            ),
                        ));
                    }
                    Some(bytes)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(read_error(self.root, &path, error)),
            };
            self.sources.insert(path.clone(), contents);
        }
        let Some(bytes) = self.sources[&path].as_ref() else {
            return Ok(());
        };
        let Ok(document) = parse_local_schema(bytes, &path) else {
            return Ok(());
        };
        let schema = if select_fragment && !fragment.is_empty() {
            if !fragment.starts_with('/') {
                return Ok(());
            }
            let Some(selected) = document.pointer(fragment) else {
                return Ok(());
            };
            selected
        } else {
            &document
        };
        self.references(schema, &path, depth + 1)
    }
}

fn read_error(root: &Path, path: &Path, source: std::io::Error) -> MdbaseRecordCacheError {
    MdbaseRecordCacheError::Read {
        path: root.join(path),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn capture(root: &Path, wrapper: &str) -> Result<SchemaSources, MdbaseRecordCacheError> {
        schema_sources(
            root,
            [(
                PathBuf::from("_types/task.md"),
                format!("---\nschema: {wrapper}\n---\n"),
            )]
            .into_iter(),
            &ControlAccess::new(None),
        )
    }

    #[test]
    fn follows_only_references_and_preserves_invalid_and_missing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("schemas")).unwrap();
        let schema = "$defs:\n  used:\n    allOf: [{$ref: first.txt}, {$ref: absent.yaml}]\n  unused: {$ref: ignored.json}\n";
        fs::write(dir.path().join("schemas/main.yaml"), schema).unwrap();
        fs::write(dir.path().join("schemas/first.txt"), "invalid: [").unwrap();
        fs::write(dir.path().join("unrelated.json"), [0xff]).unwrap();
        let sources = capture(dir.path(), "{ref: ../schemas/main.yaml#/$defs/used}").unwrap();
        assert_eq!(sources.len(), 3);
        assert_eq!(
            sources[Path::new("schemas/main.yaml")].as_deref(),
            Some(schema.as_bytes())
        );
        assert_eq!(
            sources[Path::new("schemas/first.txt")].as_deref(),
            Some(b"invalid: [".as_slice())
        );
        assert_eq!(sources[Path::new("schemas/absent.yaml")], None);
        fs::write(dir.path().join("schemas/first.txt"), "$ref: second.json").unwrap();
        let repaired = capture(dir.path(), "{ref: ../schemas/main.yaml#/$defs/used}").unwrap();
        assert_eq!(repaired[Path::new("schemas/second.json")], None);
        assert_eq!(repaired.len(), 4);
    }

    #[test]
    fn cyclic_dependencies_terminate_and_escaping_references_are_not_probed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.yaml"), "$ref: b.txt").unwrap();
        fs::write(dir.path().join("b.txt"), "$ref: a.yaml").unwrap();
        assert_eq!(capture(dir.path(), "{ref: ../a.yaml}").unwrap().len(), 2);
        for reference in [
            "../../outside.yaml",
            "https://example.invalid/schema.json",
            "urn:example:foo",
        ] {
            assert!(capture(dir.path(), &format!("{{ref: '{reference}'}}"))
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn traversal_and_read_limits_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..=MDBASE_SCHEMA_MAX_DEPTH + 1 {
            fs::write(
                dir.path().join(format!("{index}.yaml")),
                format!("$ref: {}.yaml", index + 1),
            )
            .unwrap();
        }
        assert!(capture(dir.path(), "{ref: ../0.yaml}")
            .unwrap_err()
            .to_string()
            .contains("traversal limit"));
        fs::write(
            dir.path().join("large.yaml"),
            vec![b' '; usize::try_from(MDBASE_SCHEMA_MAX_BYTES).unwrap() + 1],
        )
        .unwrap();
        assert!(capture(dir.path(), "{ref: ../large.yaml}")
            .unwrap_err()
            .to_string()
            .contains("byte limit"));
        let references = (0..=MDBASE_SCHEMA_MAX_FILES)
            .map(|index| serde_json::json!({"$ref":format!("../missing-{index}.yaml")}))
            .collect::<Vec<_>>();
        assert!(capture(
            dir.path(),
            &serde_json::json!({"value":{"allOf":references}}).to_string()
        )
        .unwrap_err()
        .to_string()
        .contains("traversal limit"));
    }

    #[test]
    fn all_contract_subject_wrappers_track_their_local_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let mut frontmatter = serde_json::Map::new();
        for key in [
            "record_schema",
            "binding_schema",
            "data_schema",
            "source_schema",
            "input_schema",
            "output_schema",
            "error_schema",
            "provider_schema",
        ] {
            frontmatter.insert(
                key.to_string(),
                serde_json::json!({"value":{"$ref":format!("../{key}.txt")}}),
            );
        }
        let source = format!("---\n{}---\n", serde_yaml::to_string(&frontmatter).unwrap());
        let sources = schema_sources(
            dir.path(),
            [(PathBuf::from("_contracts/contract.md"), source)].into_iter(),
            &ControlAccess::new(None),
        )
        .unwrap();
        assert_eq!(sources.len(), 8);
        assert!(sources.values().all(Option::is_none));
    }

    #[cfg(unix)]
    #[test]
    fn reference_symlinks_are_rejected_even_when_the_target_is_inside() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("real.yaml"), "{}").unwrap();
        std::os::unix::fs::symlink("real.yaml", dir.path().join("alias.yaml")).unwrap();
        assert!(capture(dir.path(), "{ref: ../alias.yaml}").is_err());
    }
}
