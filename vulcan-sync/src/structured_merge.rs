//! Deterministic structured three-way mergers used after Git reports overlap.

use crate::MergeFileKind;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

const MAX_STRUCTURED_DEPTH: usize = 64;
const MAX_STRUCTURED_BYTES: usize = 16 * 1024 * 1024;

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
    for (side, data) in [("base", base), ("local", local), ("remote", remote)] {
        if data.is_some_and(|data| data.len() > MAX_STRUCTURED_BYTES) {
            return Err(format!(
                "{side} content exceeds the {MAX_STRUCTURED_BYTES} byte structured merge limit"
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
    let merged = match kind {
        MergeFileKind::Json | MergeFileKind::Canvas | MergeFileKind::ObsidianState => {
            merge_json_bytes(base, local, remote, local_identity, remote_identity)?
        }
        MergeFileKind::Bases => {
            merge_yaml_bytes(base, local, remote, local_identity, remote_identity)?
        }
        MergeFileKind::Markdown => {
            merge_markdown_bytes(base, local, remote, local_identity, remote_identity)?
        }
        _ => return Ok(StructuredMergeOutcome::Unresolved),
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
            let value: serde_yaml::Value =
                serde_yaml::from_slice(data).map_err(|error| error.to_string())?;
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
        .map(serde_yaml::from_slice::<serde_yaml::Value>)
        .transpose()
        .map_err(|error| error.to_string())?
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| error.to_string())?;
    let local = serde_json::to_value(
        serde_yaml::from_slice::<serde_yaml::Value>(local).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let remote = serde_json::to_value(
        serde_yaml::from_slice::<serde_yaml::Value>(remote).map_err(|error| error.to_string())?,
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
    let yaml: serde_yaml::Value =
        serde_json::from_value(merged).map_err(|error| error.to_string())?;
    serde_yaml::to_string(&yaml)
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
    let mut output = String::new();
    if let Some(frontmatter) = frontmatter {
        let yaml: serde_yaml::Value =
            serde_json::from_value(frontmatter).map_err(|error| error.to_string())?;
        output.push_str("---\n");
        output.push_str(&serde_yaml::to_string(&yaml).map_err(|error| error.to_string())?);
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
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(&rest[..end])
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
        (Some(Value::Array(base)), Some(Value::Array(local)), Some(Value::Array(remote))) => Ok(
            merge_arrays(base, local, remote, local_identity, remote_identity)
                .map_or(MergeOutcome::Unresolved, |value| {
                    MergeOutcome::Resolved(Some(Value::Array(value)))
                }),
        ),
        (None, Some(Value::Array(local)), Some(Value::Array(remote))) => {
            Ok(
                merge_arrays(&[], local, remote, local_identity, remote_identity)
                    .map_or(MergeOutcome::Unresolved, |value| {
                        MergeOutcome::Resolved(Some(Value::Array(value)))
                    }),
            )
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
) -> Option<Vec<Value>> {
    if arrays_have_stable_ids(base, local, remote) {
        return merge_keyed_arrays(base, local, remote, local_identity, remote_identity);
    }
    if !local.starts_with(base) || !remote.starts_with(base) {
        return None;
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
    Some(merged)
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
) -> Option<Vec<Value>> {
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

    let base = keyed(base)?;
    let local = keyed(local)?;
    let remote = keyed(remote)?;
    let ids = base
        .keys()
        .chain(local.keys())
        .chain(remote.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    let mut merged = Vec::new();
    for id in ids {
        match merge_value(
            base.get(id).copied(),
            local.get(id).copied(),
            remote.get(id).copied(),
            local_identity,
            remote_identity,
            1,
        )
        .ok()?
        {
            MergeOutcome::Resolved(Some(value)) => merged.push(value),
            MergeOutcome::Resolved(None) => {}
            MergeOutcome::Unresolved => return None,
        }
    }
    Some(merged)
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
