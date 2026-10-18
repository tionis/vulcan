//! Deterministic structured three-way mergers used after Git reports overlap.
//!
//! The mergers form a closed, versioned registry ([`ARTIFACT_MERGERS`]).
//! Each entry declares what it accepts and promises; sync records the entry
//! that resolved a path. Packages and vault configuration can only select
//! among these entries through the merge policy; they never supply merge
//! code. A merger's `version` changes whenever the bytes it produces for
//! some input could change.

use crate::MergeFileKind;
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

const MAX_STRUCTURED_DEPTH: usize = 64;
const MAX_STRUCTURED_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergerFormat {
    JsonTree,
    Yaml,
    MarkdownFrontmatter,
}

/// One built-in deterministic merger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ArtifactMerger {
    /// Stable identifier, recorded with every resolution it makes.
    pub id: &'static str,
    pub version: u32,
    /// The classified file kinds this merger handles.
    pub kinds: &'static [MergeFileKind],
    /// Whether the result is the same whichever side is called local. None
    /// of the built-in mergers is: concurrent appends to the same list are
    /// ordered by the two candidates' commit IDs, so they stay internal to
    /// sync, which knows those IDs, and are not offered as a Git merge
    /// driver, which does not.
    pub role_independent: bool,
    /// Whether a common ancestor is required. Without one, only values both
    /// sides agree on, or keyed and set-like entries, merge.
    pub requires_base: bool,
    /// Largest accepted input per side, in bytes.
    pub max_bytes: usize,
    /// Deepest accepted nesting of objects and arrays.
    pub max_depth: usize,
    /// How the output is written.
    pub output: &'static str,
    /// How deletions are treated.
    pub deletion: &'static str,
    /// What the output must satisfy before it is accepted.
    pub validation: &'static str,
    #[serde(skip)]
    format: MergerFormat,
}

const DELETION: &str = "a file or entry deleted on one side is removed only when the other side left it unchanged; deleted on one side and changed on the other stays unresolved";

/// The closed registry of built-in mergers, in lookup order.
pub const ARTIFACT_MERGERS: &[ArtifactMerger] = &[
    ArtifactMerger {
        id: "vulcan.json-tree",
        version: 1,
        kinds: &[MergeFileKind::Json, MergeFileKind::ObsidianState],
        role_independent: false,
        requires_base: false,
        max_bytes: MAX_STRUCTURED_BYTES,
        max_depth: MAX_STRUCTURED_DEPTH,
        output: "pretty-printed JSON with object keys sorted, ending in a newline",
        deletion: DELETION,
        validation: "parses as JSON",
        format: MergerFormat::JsonTree,
    },
    ArtifactMerger {
        id: "vulcan.json-canvas",
        version: 1,
        kinds: &[MergeFileKind::Canvas],
        role_independent: false,
        requires_base: false,
        max_bytes: MAX_STRUCTURED_BYTES,
        max_depth: MAX_STRUCTURED_DEPTH,
        output: "pretty-printed JSON with object keys sorted, ending in a newline",
        deletion: DELETION,
        validation: "a JSON Canvas object whose `nodes` and `edges` arrays hold entries with non-empty string IDs",
        format: MergerFormat::JsonTree,
    },
    ArtifactMerger {
        id: "vulcan.bases-yaml",
        version: 1,
        kinds: &[MergeFileKind::Bases],
        role_independent: false,
        requires_base: false,
        max_bytes: MAX_STRUCTURED_BYTES,
        max_depth: MAX_STRUCTURED_DEPTH,
        output: "YAML re-emitted from the merged document; comments and formatting are not kept",
        deletion: DELETION,
        validation: "a YAML mapping",
        format: MergerFormat::Yaml,
    },
    ArtifactMerger {
        id: "vulcan.markdown-frontmatter",
        version: 1,
        kinds: &[MergeFileKind::Markdown],
        role_independent: false,
        requires_base: false,
        max_bytes: MAX_STRUCTURED_BYTES,
        max_depth: MAX_STRUCTURED_DEPTH,
        output: "frontmatter re-emitted as YAML between `---` lines, then the body; line endings become LF",
        deletion: DELETION,
        validation: "frontmatter is a mapping; the body changed on at most one side",
        format: MergerFormat::MarkdownFrontmatter,
    },
];

/// The merger registered for `kind`, if any.
#[must_use]
pub fn artifact_merger_for(kind: MergeFileKind) -> Option<&'static ArtifactMerger> {
    ARTIFACT_MERGERS
        .iter()
        .find(|merger| merger.kinds.contains(&kind))
}

#[derive(Debug, Clone, PartialEq)]
enum MergeOutcome {
    Resolved(Option<Value>),
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StructuredMergeOutcome {
    Resolved(Option<Vec<u8>>),
    Unresolved,
}

pub(crate) fn merge_structured_path(
    kind: MergeFileKind,
    base: Option<&[u8]>,
    local: Option<&[u8]>,
    remote: Option<&[u8]>,
    local_identity: &str,
    remote_identity: &str,
) -> Result<StructuredMergeOutcome, String> {
    let Some(entry) = artifact_merger_for(kind) else {
        return Ok(StructuredMergeOutcome::Unresolved);
    };
    let max_bytes = entry.max_bytes;
    for (side, data) in [("base", base), ("local", local), ("remote", remote)] {
        if data.is_some_and(|data| data.len() > max_bytes) {
            return Err(format!(
                "{side} content exceeds the {max_bytes} byte structured merge limit"
            ));
        }
    }
    if local == remote {
        return Ok(StructuredMergeOutcome::Resolved(
            local.map(ToOwned::to_owned),
        ));
    }
    if local == base {
        return Ok(StructuredMergeOutcome::Resolved(
            remote.map(ToOwned::to_owned),
        ));
    }
    if remote == base {
        return Ok(StructuredMergeOutcome::Resolved(
            local.map(ToOwned::to_owned),
        ));
    }
    let (Some(local), Some(remote)) = (local, remote) else {
        return Ok(StructuredMergeOutcome::Unresolved);
    };
    let merged = match entry.format {
        MergerFormat::JsonTree => {
            merge_json_bytes(base, local, remote, local_identity, remote_identity)?
        }
        MergerFormat::Yaml => {
            merge_yaml_bytes(base, local, remote, local_identity, remote_identity)?
        }
        MergerFormat::MarkdownFrontmatter => {
            merge_markdown_bytes(base, local, remote, local_identity, remote_identity)?
        }
    };
    let Some(data) = merged else {
        return Ok(StructuredMergeOutcome::Unresolved);
    };
    validate_merged_shape(kind, &data)?;
    Ok(StructuredMergeOutcome::Resolved(Some(data)))
}

fn validate_merged_shape(kind: MergeFileKind, data: &[u8]) -> Result<(), String> {
    match kind {
        MergeFileKind::Canvas => {
            let value: Value = serde_json::from_slice(data).map_err(|error| error.to_string())?;
            let root = value
                .as_object()
                .ok_or_else(|| "JSON Canvas root must be an object".to_string())?;
            for field in ["nodes", "edges"] {
                let entries = root
                    .get(field)
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("JSON Canvas `{field}` must be an array"))?;
                if entries.iter().any(|entry| {
                    entry
                        .as_object()
                        .and_then(|entry| entry.get("id"))
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                }) {
                    return Err(format!(
                        "JSON Canvas `{field}` entries must have non-empty string IDs"
                    ));
                }
            }
        }
        MergeFileKind::Bases => {
            let value: yaml_serde::Value =
                yaml_serde::from_slice(data).map_err(|error| error.to_string())?;
            if !value.is_mapping() {
                return Err("Bases document root must be a mapping".to_string());
            }
        }
        _ => {}
    }
    Ok(())
}

fn merge_json_bytes(
    base: Option<&[u8]>,
    local: &[u8],
    remote: &[u8],
    local_identity: &str,
    remote_identity: &str,
) -> Result<Option<Vec<u8>>, String> {
    let base = base
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|error| error.to_string())?;
    let local = serde_json::from_slice(local).map_err(|error| error.to_string())?;
    let remote = serde_json::from_slice(remote).map_err(|error| error.to_string())?;
    let MergeOutcome::Resolved(Some(merged)) = merge_value(
        base.as_ref(),
        Some(&local),
        Some(&remote),
        local_identity,
        remote_identity,
        0,
    )?
    else {
        return Ok(None);
    };
    let mut data = serde_json::to_vec_pretty(&merged).map_err(|error| error.to_string())?;
    data.push(b'\n');
    Ok(Some(data))
}

fn merge_yaml_bytes(
    base: Option<&[u8]>,
    local: &[u8],
    remote: &[u8],
    local_identity: &str,
    remote_identity: &str,
) -> Result<Option<Vec<u8>>, String> {
    let base = base
        .map(yaml_serde::from_slice::<yaml_serde::Value>)
        .transpose()
        .map_err(|error| error.to_string())?
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| error.to_string())?;
    let local = serde_json::to_value(
        yaml_serde::from_slice::<yaml_serde::Value>(local).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let remote = serde_json::to_value(
        yaml_serde::from_slice::<yaml_serde::Value>(remote).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let MergeOutcome::Resolved(Some(merged)) = merge_value(
        base.as_ref(),
        Some(&local),
        Some(&remote),
        local_identity,
        remote_identity,
        0,
    )?
    else {
        return Ok(None);
    };
    let yaml: yaml_serde::Value =
        serde_json::from_value(merged).map_err(|error| error.to_string())?;
    yaml_serde::to_string(&yaml)
        .map(String::into_bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

fn merge_markdown_bytes(
    base: Option<&[u8]>,
    local: &[u8],
    remote: &[u8],
    local_identity: &str,
    remote_identity: &str,
) -> Result<Option<Vec<u8>>, String> {
    let base = base
        .map(std::str::from_utf8)
        .transpose()
        .map_err(|error| error.to_string())?
        .map(split_frontmatter)
        .transpose()?;
    let local = split_frontmatter(std::str::from_utf8(local).map_err(|error| error.to_string())?)?;
    let remote =
        split_frontmatter(std::str::from_utf8(remote).map_err(|error| error.to_string())?)?;
    let Some(body) = merge_scalar(
        base.as_ref().map(|parts| parts.1.as_str()),
        Some(local.1.as_str()),
        Some(remote.1.as_str()),
    ) else {
        return Ok(None);
    };
    let MergeOutcome::Resolved(frontmatter) = merge_value(
        base.as_ref().and_then(|parts| parts.0.as_ref()),
        local.0.as_ref(),
        remote.0.as_ref(),
        local_identity,
        remote_identity,
        0,
    )?
    else {
        return Ok(None);
    };
    if frontmatter.as_ref().is_some_and(|value| !value.is_object()) {
        return Err("merged Markdown frontmatter must be a mapping".to_string());
    }
    let mut output = String::new();
    if let Some(frontmatter) = frontmatter {
        let yaml: yaml_serde::Value =
            serde_json::from_value(frontmatter).map_err(|error| error.to_string())?;
        output.push_str("---\n");
        output.push_str(&yaml_serde::to_string(&yaml).map_err(|error| error.to_string())?);
        output.push_str("---\n");
    }
    output.push_str(body);
    Ok(Some(output.into_bytes()))
}

fn split_frontmatter(input: &str) -> Result<(Option<Value>, String), String> {
    let normalized = input.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        return Ok((None, normalized));
    }
    let rest = &normalized[4..];
    let Some(end) = rest.find("\n---\n") else {
        return Err("unterminated Markdown YAML frontmatter".to_string());
    };
    let yaml = yaml_serde::from_str::<yaml_serde::Value>(&rest[..end])
        .map_err(|error| error.to_string())?;
    let value = serde_json::to_value(yaml).map_err(|error| error.to_string())?;
    Ok((Some(value), rest[end + 5..].to_string()))
}

fn merge_value(
    base: Option<&Value>,
    local: Option<&Value>,
    remote: Option<&Value>,
    local_identity: &str,
    remote_identity: &str,
    depth: usize,
) -> Result<MergeOutcome, String> {
    if depth > MAX_STRUCTURED_DEPTH {
        return Err(format!(
            "structured merge exceeds the {MAX_STRUCTURED_DEPTH} level depth limit"
        ));
    }
    if local == remote {
        return Ok(MergeOutcome::Resolved(local.cloned()));
    }
    if local == base {
        return Ok(MergeOutcome::Resolved(remote.cloned()));
    }
    if remote == base {
        return Ok(MergeOutcome::Resolved(local.cloned()));
    }
    match (base, local, remote) {
        (base, Some(Value::Object(local)), Some(Value::Object(remote))) => merge_objects(
            base.and_then(Value::as_object),
            local,
            remote,
            local_identity,
            remote_identity,
            depth + 1,
        ),
        (Some(Value::Array(base)), Some(Value::Array(local)), Some(Value::Array(remote))) => {
            let merged = merge_arrays(base, local, remote, local_identity, remote_identity, depth)?;
            Ok(merged.map_or(MergeOutcome::Unresolved, |value| {
                MergeOutcome::Resolved(Some(Value::Array(value)))
            }))
        }
        (None, Some(Value::Array(local)), Some(Value::Array(remote))) => {
            let merged = merge_arrays(&[], local, remote, local_identity, remote_identity, depth)?;
            Ok(merged.map_or(MergeOutcome::Unresolved, |value| {
                MergeOutcome::Resolved(Some(Value::Array(value)))
            }))
        }
        _ => Ok(MergeOutcome::Unresolved),
    }
}

fn merge_objects(
    base: Option<&Map<String, Value>>,
    local: &Map<String, Value>,
    remote: &Map<String, Value>,
    local_identity: &str,
    remote_identity: &str,
    depth: usize,
) -> Result<MergeOutcome, String> {
    let keys = base
        .into_iter()
        .flat_map(Map::keys)
        .chain(local.keys())
        .chain(remote.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut merged = Map::new();
    for key in keys {
        let value = merge_value(
            base.and_then(|base| base.get(&key)),
            local.get(&key),
            remote.get(&key),
            local_identity,
            remote_identity,
            depth,
        )?;
        match value {
            MergeOutcome::Resolved(Some(value)) => {
                merged.insert(key, value);
            }
            MergeOutcome::Resolved(None) => {}
            MergeOutcome::Unresolved => return Ok(MergeOutcome::Unresolved),
        }
    }
    Ok(MergeOutcome::Resolved(Some(Value::Object(merged))))
}

fn merge_arrays(
    base: &[Value],
    local: &[Value],
    remote: &[Value],
    local_identity: &str,
    remote_identity: &str,
    depth: usize,
) -> Result<Option<Vec<Value>>, String> {
    if arrays_have_stable_ids(base, local, remote) {
        return merge_keyed_arrays(base, local, remote, local_identity, remote_identity, depth);
    }
    if let Some(merged) = merge_string_sets(base, local, remote, local_identity, remote_identity) {
        return Ok(Some(merged));
    }
    if !local.starts_with(base) || !remote.starts_with(base) {
        return Ok(None);
    }
    let (first, second) = if local_identity <= remote_identity {
        (&local[base.len()..], &remote[base.len()..])
    } else {
        (&remote[base.len()..], &local[base.len()..])
    };
    let mut merged = base.to_vec();
    for value in first.iter().chain(second) {
        if !merged.contains(value) {
            merged.push(value.clone());
        }
    }
    Ok(Some(merged))
}

/// Merges arrays of distinct strings, such as enabled plugins, tags, or
/// column lists, as ordered sets: an entry either side removed is removed,
/// entries either side added are appended in an order independent of the
/// local and remote roles, and one side's reordering of the surviving entries
/// is kept. Returns `None` for other arrays and for conflicting reorders.
fn merge_string_sets<'a>(
    base: &'a [Value],
    local: &'a [Value],
    remote: &'a [Value],
    local_identity: &str,
    remote_identity: &str,
) -> Option<Vec<Value>> {
    fn distinct_strings(values: &[Value]) -> Option<(Vec<&str>, BTreeSet<&str>)> {
        let mut order = Vec::with_capacity(values.len());
        let mut set = BTreeSet::new();
        for value in values {
            let entry = value.as_str()?;
            if !set.insert(entry) {
                return None;
            }
            order.push(entry);
        }
        Some((order, set))
    }

    let (base, base_set) = distinct_strings(base)?;
    let (local, local_set) = distinct_strings(local)?;
    let (remote, remote_set) = distinct_strings(remote)?;
    let survivor_sets = [&base_set, &local_set, &remote_set];
    let surviving = |order: &[&'a str]| -> Vec<&'a str> {
        order
            .iter()
            .copied()
            .filter(|entry| survivor_sets.iter().all(|set| set.contains(entry)))
            .collect()
    };
    let (in_base, in_local, in_remote) = (surviving(&base), surviving(&local), surviving(&remote));
    let mut merged = if in_local == in_base {
        in_remote
    } else if in_remote == in_base || in_remote == in_local {
        in_local
    } else {
        return None;
    };
    let (first, second) = if local_identity <= remote_identity {
        (&local, &remote)
    } else {
        (&remote, &local)
    };
    let mut added = BTreeSet::new();
    for entry in first.iter().chain(second.iter()) {
        if !base_set.contains(entry) && added.insert(*entry) {
            merged.push(entry);
        }
    }
    Some(
        merged
            .into_iter()
            .map(|entry| Value::String(entry.to_string()))
            .collect(),
    )
}

fn arrays_have_stable_ids(base: &[Value], local: &[Value], remote: &[Value]) -> bool {
    !local.is_empty()
        && !remote.is_empty()
        && base
            .iter()
            .chain(local)
            .chain(remote)
            .all(|value| value.get("id").and_then(Value::as_str).is_some())
}

fn merge_keyed_arrays(
    base: &[Value],
    local: &[Value],
    remote: &[Value],
    local_identity: &str,
    remote_identity: &str,
    depth: usize,
) -> Result<Option<Vec<Value>>, String> {
    fn keyed(values: &[Value]) -> Option<std::collections::BTreeMap<&str, &Value>> {
        let mut result = std::collections::BTreeMap::new();
        for value in values {
            let id = value.get("id")?.as_str()?;
            if result.insert(id, value).is_some() {
                return None;
            }
        }
        Some(result)
    }

    let missing_id = || "keyed merge requires unique string entry ids".to_string();
    let base_map = keyed(base).ok_or_else(missing_id)?;
    let local_map = keyed(local).ok_or_else(missing_id)?;
    let remote_map = keyed(remote).ok_or_else(missing_id)?;
    // Preserve the base order for surviving entries so order-sensitive
    // arrays are not rewritten; concurrently added entries follow in a
    // deterministic sorted order that does not depend on local/remote roles.
    let mut ordered_ids = Vec::new();
    let mut seen = BTreeSet::new();
    for value in base {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .expect("stable ids are checked before keyed merging");
        if seen.insert(id) {
            ordered_ids.push(id);
        }
    }
    let mut added_ids: BTreeSet<&str> =
        local_map.keys().chain(remote_map.keys()).copied().collect();
    added_ids.retain(|id| !base_map.contains_key(id));
    ordered_ids.extend(added_ids);
    let mut merged = Vec::new();
    for id in ordered_ids {
        match merge_value(
            base_map.get(id).copied(),
            local_map.get(id).copied(),
            remote_map.get(id).copied(),
            local_identity,
            remote_identity,
            depth + 1,
        )? {
            MergeOutcome::Resolved(Some(value)) => merged.push(value),
            MergeOutcome::Resolved(None) => {}
            MergeOutcome::Unresolved => return Ok(None),
        }
    }
    Ok(Some(merged))
}

fn merge_scalar<'a, T: PartialEq + ?Sized>(
    base: Option<&'a T>,
    local: Option<&'a T>,
    remote: Option<&'a T>,
) -> Option<&'a T> {
    if local == remote {
        local
    } else if local == base {
        remote
    } else if remote == base {
        local
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_closed_and_unambiguous() {
        let mut ids = BTreeSet::new();
        let mut claimed = BTreeSet::new();
        for merger in ARTIFACT_MERGERS {
            assert!(ids.insert(merger.id), "duplicate merger {}", merger.id);
            assert!(merger.id.starts_with("vulcan."));
            assert!(merger.version >= 1);
            for kind in merger.kinds {
                assert!(
                    claimed.insert(format!("{kind:?}")),
                    "{kind:?} has two mergers"
                );
                assert_eq!(artifact_merger_for(*kind), Some(merger));
            }
        }
        for kind in [
            MergeFileKind::Text,
            MergeFileKind::Binary,
            MergeFileKind::Missing,
        ] {
            assert!(artifact_merger_for(kind).is_none(), "{kind:?}");
            assert_eq!(
                merge_structured_path(kind, Some(b"a"), Some(b"b"), Some(b"c"), "1", "2"),
                Ok(StructuredMergeOutcome::Unresolved)
            );
        }
        let listed = serde_json::to_value(ARTIFACT_MERGERS).expect("registry JSON");
        assert_eq!(listed[0]["id"], "vulcan.json-tree");
        assert!(listed[0].get("format").is_none());
    }

    /// No built-in merger may claim role independence while its output
    /// depends on which side is local: concurrent appends are ordered by
    /// the candidates' identities.
    #[test]
    fn role_independence_is_declared_truthfully() {
        type Case = (MergeFileKind, &'static [u8], &'static [u8], &'static [u8]);
        let inputs: [Case; 4] = [
            (MergeFileKind::Json, b"[1]", b"[1,2]", b"[1,3]"),
            (
                MergeFileKind::Canvas,
                b"{\"nodes\":[],\"edges\":[],\"x\":[1]}",
                b"{\"nodes\":[],\"edges\":[],\"x\":[1,2]}",
                b"{\"nodes\":[],\"edges\":[],\"x\":[1,3]}",
            ),
            (
                MergeFileKind::Bases,
                b"x: [1]\n",
                b"x: [1, 2]\n",
                b"x: [1, 3]\n",
            ),
            (
                MergeFileKind::Markdown,
                b"---\nx: [1]\n---\nBody\n",
                b"---\nx: [1, 2]\n---\nBody\n",
                b"---\nx: [1, 3]\n---\nBody\n",
            ),
        ];
        for (kind, base, local, remote) in inputs {
            let merger = artifact_merger_for(kind).expect("registered");
            let forward = resolved(merge_structured_path(
                kind,
                Some(base),
                Some(local),
                Some(remote),
                "a",
                "b",
            ));
            let relabelled = resolved(merge_structured_path(
                kind,
                Some(base),
                Some(remote),
                Some(local),
                "a",
                "b",
            ));
            assert!(!merger.role_independent, "{}", merger.id);
            assert_ne!(
                forward, relabelled,
                "{} no longer depends on roles",
                merger.id
            );
            // Swapping the identities with the sides gives the same bytes.
            let swapped = resolved(merge_structured_path(
                kind,
                Some(base),
                Some(remote),
                Some(local),
                "b",
                "a",
            ));
            assert_eq!(forward, swapped, "{}", merger.id);
        }
    }

    fn resolved(result: Result<StructuredMergeOutcome, String>) -> Option<Vec<u8>> {
        match result.expect("merge") {
            StructuredMergeOutcome::Resolved(data) => data,
            StructuredMergeOutcome::Unresolved => panic!("expected a resolved merge"),
        }
    }

    #[test]
    fn json_merges_disjoint_keys_and_orders_concurrent_appends_by_oid() {
        let merged = resolved(merge_structured_path(
            MergeFileKind::Json,
            Some(br#"{"base":true,"items":[1]}"#),
            Some(br#"{"base":true,"local":1,"items":[1,2]}"#),
            Some(br#"{"base":true,"remote":2,"items":[1,3]}"#),
            "b",
            "a",
        ))
        .expect("content");
        let value: Value = serde_json::from_slice(&merged).expect("JSON");
        assert_eq!(value["local"], 1);
        assert_eq!(value["remote"], 2);
        assert_eq!(value["items"], serde_json::json!([1, 3, 2]));
    }

    #[test]
    fn structured_addition_order_is_independent_of_local_and_remote_roles() {
        let base = Some(br#"{"items":[1]}"#.as_slice());
        let candidate_a = Some(br#"{"items":[1,3]}"#.as_slice());
        let candidate_b = Some(br#"{"items":[1,2]}"#.as_slice());
        let first = resolved(merge_structured_path(
            MergeFileKind::Json,
            base,
            candidate_b,
            candidate_a,
            "b",
            "a",
        ));
        let swapped = resolved(merge_structured_path(
            MergeFileKind::Json,
            base,
            candidate_a,
            candidate_b,
            "a",
            "b",
        ));
        assert_eq!(first, swapped);
        assert_eq!(
            serde_json::from_slice::<Value>(&first.expect("content")).expect("JSON"),
            serde_json::json!({"items": [1, 3, 2]})
        );
    }

    #[test]
    fn string_arrays_merge_as_ordered_sets() {
        let merge = |base: &str, local: &str, remote: &str, local_identity: &str| {
            let remote_identity = if local_identity == "a" { "b" } else { "a" };
            match merge_structured_path(
                MergeFileKind::ObsidianState,
                Some(base.as_bytes()),
                Some(local.as_bytes()),
                Some(remote.as_bytes()),
                local_identity,
                remote_identity,
            )
            .expect("merge")
            {
                StructuredMergeOutcome::Resolved(data) => {
                    Some(serde_json::from_slice::<Value>(&data.expect("content")).expect("JSON"))
                }
                StructuredMergeOutcome::Unresolved => None,
            }
        };
        // One device disables a plugin while another enables two.
        let base = r#"["dataview","templater","kanban"]"#;
        let local = r#"["dataview","kanban"]"#;
        let remote = r#"["dataview","templater","kanban","tasks","calendar"]"#;
        let expected = serde_json::json!(["dataview", "kanban", "tasks", "calendar"]);
        assert_eq!(merge(base, local, remote, "a"), Some(expected.clone()));
        // Swapping the roles gives the same bytes.
        assert_eq!(merge(base, remote, local, "b"), Some(expected));
        // Concurrent additions follow the identity order, once each.
        assert_eq!(
            merge(r#"["a"]"#, r#"["a","x","y"]"#, r#"["a","y","z"]"#, "b"),
            Some(serde_json::json!(["a", "y", "z", "x"]))
        );
        // One side's reorder survives the other side's addition and removal.
        assert_eq!(
            merge(
                r#"["a","b","c"]"#,
                r#"["c","b","a"]"#,
                r#"["a","c","d"]"#,
                "a"
            ),
            Some(serde_json::json!(["c", "a", "d"]))
        );
        // Both sides reordering differently needs review.
        assert_eq!(
            merge(
                r#"["a","b","c"]"#,
                r#"["c","b","a"]"#,
                r#"["b","a","c"]"#,
                "a"
            ),
            None
        );
        // Arrays with duplicates or non-strings keep append-only semantics.
        assert_eq!(
            merge(r#"["a","a"]"#, r#"["a"]"#, r#"["a","a","b"]"#, "a"),
            None
        );
        assert_eq!(merge("[1,2]", "[2]", "[1,2,3]", "a"), None);
        // Frontmatter lists use the same rule.
        let merged = resolved(merge_structured_path(
            MergeFileKind::Markdown,
            Some(b"---\ntags: [a, b]\n---\nbody\n"),
            Some(b"---\ntags: [b]\n---\nbody\n"),
            Some(b"---\ntags: [a, b, c]\n---\nbody\n"),
            "a",
            "b",
        ))
        .expect("content");
        assert_eq!(
            String::from_utf8(merged).expect("UTF-8"),
            "---\ntags:\n- b\n- c\n---\nbody\n"
        );
    }

    #[test]
    fn keyed_arrays_preserve_base_order_and_sort_concurrent_additions() {
        let merged = resolved(merge_structured_path(
            MergeFileKind::Json,
            Some(br#"{"items":[{"id":"b","v":0},{"id":"a","v":0}]}"#),
            Some(br#"{"items":[{"id":"b","v":1},{"id":"a","v":0},{"id":"c","v":0}]}"#),
            Some(br#"{"items":[{"id":"b","v":0},{"id":"a","v":2},{"id":"d","v":0}]}"#),
            "local",
            "remote",
        ))
        .expect("content");
        let value: Value = serde_json::from_slice(&merged).expect("JSON");
        let ids = value["items"]
            .as_array()
            .expect("items array")
            .iter()
            .map(|entry| entry["id"].as_str().expect("id").to_string())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["b", "a", "c", "d"]);
        assert_eq!(value["items"][0]["v"], 1);
        assert_eq!(value["items"][1]["v"], 2);
    }

    #[test]
    fn keyed_array_nesting_respects_the_depth_limit() {
        // Every level carries a concurrent disjoint edit on each side so no
        // fast path prunes the recursion before the depth cap trips.
        fn level(index: usize, last: usize, left: i64, right: i64) -> Value {
            let mut node = serde_json::json!({
                "id": format!("level{index}"),
                "edit_left": left,
                "edit_right": right,
            });
            if index < last {
                node["items"] = Value::Array(vec![level(index + 1, last, left, right)]);
            }
            node
        }
        // Forty object/array levels exceed this merger's depth cap of 64
        // while staying below serde_json's own 128-level recursion limit.
        let last = 40;
        let base = level(0, last, 0, 0);
        let local = level(0, last, 1, 0);
        let remote = level(0, last, 0, 1);
        let error = merge_structured_path(
            MergeFileKind::Json,
            Some(serde_json::to_vec(&base).expect("base").as_slice()),
            Some(serde_json::to_vec(&local).expect("local").as_slice()),
            Some(serde_json::to_vec(&remote).expect("remote").as_slice()),
            "local",
            "remote",
        )
        .expect_err("over-deep keyed nesting must fail");
        assert!(error.contains("depth limit"));
    }

    #[test]
    fn markdown_rejects_non_mapping_merged_frontmatter() {
        // Both sides agree on a scalar frontmatter while the body still
        // needs merging: emitting it between --- markers would produce
        // invalid frontmatter, so the merge must fail closed.
        let base = b"---\ntitle: Home\n---\n# Home\n";
        let local = b"---\n- just\n- a\n- list\n---\n# Home\n";
        let remote = b"---\n- just\n- a\n- list\n---\n# Remote\n";
        let error = merge_structured_path(
            MergeFileKind::Markdown,
            Some(base),
            Some(local),
            Some(remote),
            "local",
            "remote",
        )
        .expect_err("scalar frontmatter must not merge");
        assert!(error.contains("must be a mapping"));
    }

    #[test]
    fn markdown_merges_frontmatter_only_when_body_is_unchanged() {
        let base = b"---\ntitle: Home\ntags: [base]\n---\n# Home\n";
        let local = b"---\ntitle: Local\ntags: [base]\n---\n# Home\n";
        let remote = b"---\ntitle: Home\ntags: [base, remote]\n---\n# Home\n";
        let merged = resolved(merge_structured_path(
            MergeFileKind::Markdown,
            Some(base),
            Some(local),
            Some(remote),
            "a",
            "b",
        ))
        .expect("content");
        let text = String::from_utf8(merged).expect("UTF-8");
        assert!(text.contains("title: Local"));
        assert!(text.contains("- remote"));
        assert!(text.ends_with("# Home\n"));

        assert!(matches!(
            merge_structured_path(
                MergeFileKind::Markdown,
                Some(base),
                Some(b"---\ntitle: Local\n---\n# Local\n"),
                Some(b"---\ntitle: Remote\n---\n# Remote\n"),
                "a",
                "b",
            )
            .expect("merge"),
            StructuredMergeOutcome::Unresolved
        ));
    }

    #[test]
    fn delete_modify_and_scalar_conflicts_require_review() {
        assert!(matches!(
            merge_structured_path(
                MergeFileKind::Json,
                Some(br#"{"value":0}"#),
                None,
                Some(br#"{"value":1}"#),
                "a",
                "b",
            )
            .expect("merge"),
            StructuredMergeOutcome::Unresolved
        ));
        assert!(matches!(
            merge_structured_path(
                MergeFileKind::Json,
                Some(br#"{"value":0}"#),
                Some(br#"{"value":1}"#),
                Some(br#"{"value":2}"#),
                "a",
                "b",
            )
            .expect("merge"),
            StructuredMergeOutcome::Unresolved
        ));
    }

    #[test]
    fn object_deletions_and_markdown_without_frontmatter_are_distinct_from_conflicts() {
        let deleted = resolved(merge_structured_path(
            MergeFileKind::Json,
            Some(br#"{"remove":true,"keep":1}"#),
            Some(br#"{"keep":1}"#),
            Some(br#"{"remove":true,"keep":2}"#),
            "a",
            "b",
        ))
        .expect("content");
        assert_eq!(
            serde_json::from_slice::<Value>(&deleted).expect("JSON"),
            serde_json::json!({"keep": 2})
        );

        assert_eq!(
            resolved(merge_structured_path(
                MergeFileKind::Markdown,
                Some(b"# Base\n"),
                Some(b"# Local\n"),
                Some(b"# Base\n"),
                "a",
                "b",
            )),
            Some(b"# Local\n".to_vec())
        );
    }

    #[test]
    fn canvas_arrays_merge_objects_by_stable_id() {
        let merged = resolved(merge_structured_path(
            MergeFileKind::Canvas,
            Some(br#"{"nodes":[{"id":"n1","x":0,"y":0}],"edges":[]}"#),
            Some(br#"{"nodes":[{"id":"n1","x":1,"y":0},{"id":"n2","x":0}],"edges":[]}"#),
            Some(br#"{"nodes":[{"id":"n1","x":0,"y":2},{"id":"n3","x":0}],"edges":[]}"#),
            "a",
            "b",
        ))
        .expect("content");
        let value: Value = serde_json::from_slice(&merged).expect("JSON");
        assert_eq!(
            value["nodes"][0],
            serde_json::json!({"id":"n1","x":1,"y":2})
        );
        assert_eq!(value["nodes"][1]["id"], "n2");
        assert_eq!(value["nodes"][2]["id"], "n3");
    }

    #[test]
    fn canvas_and_bases_schema_shapes_fail_closed() {
        assert!(merge_structured_path(
            MergeFileKind::Canvas,
            Some(br#"{"nodes":[],"edges":[]}"#),
            Some(br#"{"nodes":[{"x":1}],"edges":[]}"#),
            Some(br#"{"nodes":[],"edges":[{"id":"e1"}]}"#),
            "a",
            "b",
        )
        .is_err());
        assert!(merge_structured_path(
            MergeFileKind::Bases,
            Some(b"[]\n"),
            Some(b"- local\n"),
            Some(b"- remote\n"),
            "a",
            "b",
        )
        .is_err());
    }

    #[test]
    fn explicitly_selected_obsidian_state_uses_the_bounded_json_merger() {
        let outcome = merge_structured_path(
            MergeFileKind::ObsidianState,
            Some(br#"{"base":true}"#),
            Some(br#"{"base":true,"local":1}"#),
            Some(br#"{"base":true,"remote":2}"#),
            "1111",
            "2222",
        )
        .expect("Obsidian state merge");
        let StructuredMergeOutcome::Resolved(Some(data)) = outcome else {
            panic!("selected Obsidian JSON should resolve");
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&data).expect("merged JSON"),
            serde_json::json!({"base": true, "local": 1, "remote": 2})
        );
    }
}
