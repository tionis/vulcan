//! Exact JSON resource accounting without cloning bindings or retaining encoded bytes.
use super::{MdbaseCelError, MdbaseCelLimits};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, Write};

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
enum Root<'a> {
    Value(&'a Value),
    Bindings(&'a BTreeMap<String, Value>),
}

#[derive(Default)]
struct ByteCount(usize);

impl Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encoded_size(root: Root<'_>) -> Result<usize, MdbaseCelError> {
    let mut count = ByteCount::default();
    // Use the identical compact serializer, including string/key escapes and
    // number formatting. Finish counting even past the limit: errors expose the
    // exact byte count, not the first byte that exceeded the budget.
    serde_json::to_writer(&mut count, &root)
        .map_err(|error| MdbaseCelError::new("expression_value_error", error.to_string()))?;
    Ok(count.0)
}

pub(super) fn inspect_bindings(
    bindings: &BTreeMap<String, Value>,
    limits: &MdbaseCelLimits,
) -> Result<(), MdbaseCelError> {
    // String keys and already-materialized JSON values need no fallible
    // intermediate conversion to Value. Treat the bindings map as one root node.
    inspect(Root::Bindings(bindings), limits, "input")
}

/// Exact totals that [`inspect_bindings`] compares with its limits: encoded
/// bytes, traversed nodes, and the widest map or list (including the root).
pub(super) fn measure_bindings(
    bindings: &BTreeMap<String, Value>,
) -> Result<(usize, usize, usize), MdbaseCelError> {
    let bytes = encoded_size(Root::Bindings(bindings))?;
    let mut nodes = 0_usize;
    let mut width = 0_usize;
    let mut pending = vec![Root::Bindings(bindings)];
    while let Some(value) = pending.pop() {
        nodes = nodes.saturating_add(1);
        match value {
            Root::Bindings(values) => {
                width = width.max(values.len());
                pending.extend(values.values().map(Root::Value));
            }
            Root::Value(Value::Array(values)) => {
                width = width.max(values.len());
                pending.extend(values.iter().map(Root::Value));
            }
            Root::Value(Value::Object(values)) => {
                width = width.max(values.len());
                pending.extend(values.values().map(Root::Value));
            }
            Root::Value(_) => {}
        }
    }
    Ok((bytes, nodes, width))
}

pub(super) fn inspect_value(
    value: &Value,
    limits: &MdbaseCelLimits,
    label: &str,
) -> Result<(), MdbaseCelError> {
    inspect(Root::Value(value), limits, label)
}

fn inspect(root: Root<'_>, limits: &MdbaseCelLimits, label: &str) -> Result<(), MdbaseCelError> {
    let bytes = encoded_size(root)?;
    if bytes > limits.max_value_bytes {
        return Err(MdbaseCelError::limit(
            &format!("{label} value size"),
            bytes,
            limits.max_value_bytes,
        ));
    }
    let mut nodes = 0_usize;
    let mut pending = vec![root];
    while let Some(value) = pending.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > limits.max_value_nodes {
            return Err(MdbaseCelError::limit(
                &format!("{label} value node count"),
                nodes,
                limits.max_value_nodes,
            ));
        }
        match value {
            Root::Bindings(values) => {
                inspect_width(values.len(), limits, label, "map")?;
                pending.extend(values.values().map(Root::Value));
            }
            Root::Value(Value::Array(values)) => {
                inspect_width(values.len(), limits, label, "list")?;
                pending.extend(values.iter().map(Root::Value));
            }
            Root::Value(Value::Object(values)) => {
                inspect_width(values.len(), limits, label, "map")?;
                pending.extend(values.values().map(Root::Value));
            }
            Root::Value(_) => {}
        }
    }
    Ok(())
}

fn inspect_width(
    count: usize,
    limits: &MdbaseCelLimits,
    label: &str,
    kind: &str,
) -> Result<(), MdbaseCelError> {
    if count > limits.max_collection_items {
        return Err(MdbaseCelError::limit(
            &format!("{label} {kind} iteration width"),
            count,
            limits.max_collection_items,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Frozen pre-optimization implementation, independent of the new traversal.
    fn original_inspect_value(
        value: &serde_json::Value,
        limits: &MdbaseCelLimits,
        label: &str,
    ) -> Result<(), MdbaseCelError> {
        let bytes = serde_json::to_vec(value)
            .map_err(|error| MdbaseCelError::new("expression_value_error", error.to_string()))?
            .len();
        if bytes > limits.max_value_bytes {
            return Err(MdbaseCelError::limit(
                &format!("{label} value size"),
                bytes,
                limits.max_value_bytes,
            ));
        }
        let mut nodes = 0_usize;
        let mut pending = vec![value];
        while let Some(value) = pending.pop() {
            nodes = nodes.saturating_add(1);
            if nodes > limits.max_value_nodes {
                return Err(MdbaseCelError::limit(
                    &format!("{label} value node count"),
                    nodes,
                    limits.max_value_nodes,
                ));
            }
            match value {
                serde_json::Value::Array(values) => {
                    if values.len() > limits.max_collection_items {
                        return Err(MdbaseCelError::limit(
                            &format!("{label} list iteration width"),
                            values.len(),
                            limits.max_collection_items,
                        ));
                    }
                    pending.extend(values);
                }
                serde_json::Value::Object(values) => {
                    if values.len() > limits.max_collection_items {
                        return Err(MdbaseCelError::limit(
                            &format!("{label} map iteration width"),
                            values.len(),
                            limits.max_collection_items,
                        ));
                    }
                    pending.extend(values.values());
                }
                _ => {}
            }
        }
        Ok(())
    }

    #[test]
    fn borrowed_accounting_matches_original_bytes_nodes_width_and_error_order() {
        let cases = [
            json!(null),
            json!(true),
            json!(false),
            json!(i64::MIN),
            json!(u64::MAX),
            json!(-0.0),
            json!(1.234e-100),
            json!("quotes \" slash \\ tab\t line\n nul\0 α😀"),
            json!([]),
            json!({}),
            json!([1, null, {"z": [true, false, 3]}]),
            json!({"a": [1, 2, 3], "z": {"nested": {}, "wide": [0, 1, 2, 3]}}),
        ];
        for value in cases {
            let bindings = BTreeMap::from([
                ("α\"\n".to_string(), value.clone()),
                ("unused".to_string(), json!({"body": "x".repeat(257)})),
            ]);
            let materialized = serde_json::to_value(&bindings).unwrap();
            let bytes = serde_json::to_vec(&value).unwrap().len();
            let binding_bytes = serde_json::to_vec(&materialized).unwrap().len();
            assert_eq!(encoded_size(Root::Value(&value)).unwrap(), bytes);
            assert_eq!(
                encoded_size(Root::Bindings(&bindings)).unwrap(),
                binding_bytes
            );
            for byte_limit in [
                0,
                bytes.saturating_sub(1),
                bytes,
                binding_bytes - 1,
                binding_bytes,
                usize::MAX,
            ] {
                for node_limit in [0, 1, 2, 3, 8, 100] {
                    for width_limit in [0, 1, 2, 4, 100] {
                        let limits = MdbaseCelLimits {
                            max_value_bytes: byte_limit,
                            max_value_nodes: node_limit,
                            max_collection_items: width_limit,
                            ..MdbaseCelLimits::default()
                        };
                        assert_eq!(
                            inspect_value(&value, &limits, "output"),
                            original_inspect_value(&value, &limits, "output"),
                            "output {value:?} {limits:?}"
                        );
                        assert_eq!(
                            inspect_bindings(&bindings, &limits),
                            original_inspect_value(&materialized, &limits, "input"),
                            "input {bindings:?} {limits:?}"
                        );
                    }
                }
            }
        }
        let empty = BTreeMap::new();
        for node_limit in [0, 1] {
            let limits = MdbaseCelLimits {
                max_value_nodes: node_limit,
                ..MdbaseCelLimits::default()
            };
            assert_eq!(
                inspect_bindings(&empty, &limits),
                original_inspect_value(&json!({}), &limits, "input")
            );
        }
    }

    #[test]
    fn counting_writer_retains_only_exact_byte_count() {
        let mut count = ByteCount::default();
        count.write_all(b"one").unwrap();
        count.write_all("α😀".as_bytes()).unwrap();
        count.write_all(&[]).unwrap();
        count.flush().unwrap();
        assert_eq!(count.0, 9);
        assert_eq!(
            std::mem::size_of::<ByteCount>(),
            std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn constant_programs_still_validate_all_unused_bindings_at_both_entrypoints() {
        use super::super::{
            MdbaseCelClock, MdbaseCelContext, MdbaseCelContextKind, MdbaseCelEngine,
        };
        let bindings = BTreeMap::from([("unused".into(), json!({"body": "x".repeat(128)}))]);
        let context = MdbaseCelContext {
            kind: MdbaseCelContextKind::QueryFilter,
            bindings: bindings.clone(),
            clock: MdbaseCelClock::new(chrono::Utc::now(), "UTC").unwrap(),
            path: None,
            link_index: None,
        };
        for limits in [
            MdbaseCelLimits {
                max_value_bytes: 16,
                ..MdbaseCelLimits::default()
            },
            MdbaseCelLimits {
                max_value_nodes: 1,
                ..MdbaseCelLimits::default()
            },
            MdbaseCelLimits {
                max_collection_items: 0,
                ..MdbaseCelLimits::default()
            },
        ] {
            let expected =
                original_inspect_value(&serde_json::to_value(&bindings).unwrap(), &limits, "input")
                    .unwrap_err();
            let engine = MdbaseCelEngine::new(limits);
            let program = engine.compile("true").unwrap();
            assert_eq!(engine.evaluate(&program, &bindings).unwrap_err(), expected);
            assert_eq!(
                engine.evaluate_context(&program, &context).unwrap_err(),
                expected
            );
        }
    }
}
