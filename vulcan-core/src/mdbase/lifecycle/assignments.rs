use super::{BTreeMap, MdbaseTypeCompositionDiagnostic, Value};

const MAX_TARGETS: usize = 10_000;

/// Apply lifecycle field selectors to a copy of raw frontmatter. All selectors
/// resolve against the original snapshot, so expansion and collision handling
/// cannot depend on map iteration order. Conflicting aliases and parent/child
/// assignments fail before a result is returned.
pub fn apply_mdbase_lifecycle_assignments(
    draft: &Value,
    assignments: &BTreeMap<String, Value>,
) -> Result<Value, MdbaseTypeCompositionDiagnostic> {
    if !draft.is_object() {
        return Err(error(
            "lifecycle_assignment_error",
            "",
            "draft must be an object",
        ));
    }
    let mut targets = BTreeMap::<Vec<String>, (&str, &Value)>::new();
    for (selector, value) in assignments {
        let segments = parse_selector(selector)?;
        let mut paths = Vec::new();
        expand(
            Some(draft),
            &segments,
            &mut Vec::new(),
            &mut paths,
            selector,
        )?;
        if paths.is_empty() {
            return Err(error(
                "lifecycle_assignment_error",
                selector,
                "field expansion has no targets",
            ));
        }
        for path in paths {
            if let Some((previous, previous_value)) = targets.get(&path) {
                if *previous_value != value {
                    return Err(conflict(previous, selector));
                }
            } else {
                targets.insert(path, (selector, value));
            }
            if targets.len() > MAX_TARGETS {
                return Err(error(
                    "limit_exceeded",
                    selector,
                    "too many lifecycle assignment targets",
                ));
            }
        }
    }
    let mut previous: Option<(&Vec<String>, &str)> = None;
    for (path, (selector, _)) in &targets {
        if let Some((parent, parent_selector)) = previous {
            if path.starts_with(parent) {
                return Err(conflict(parent_selector, selector));
            }
        }
        previous = Some((path, selector));
    }
    let mut result = draft.clone();
    for (path, (_, value)) in targets {
        set_value(&mut result, &path, value.clone());
    }
    Ok(result)
}

fn error(code: &str, field: &str, message: &str) -> MdbaseTypeCompositionDiagnostic {
    MdbaseTypeCompositionDiagnostic {
        code: code.to_string(),
        message: message.to_string(),
        field: field.to_string(),
        type_names: Vec::new(),
        locations: vec![field.to_string()],
    }
}

fn conflict(left: &str, right: &str) -> MdbaseTypeCompositionDiagnostic {
    let mut diagnostic = error(
        "type_conflict",
        right,
        "lifecycle assignments overlap or disagree at the same persisted field",
    );
    diagnostic.locations = vec![left.to_string(), right.to_string()];
    diagnostic.locations.sort();
    diagnostic
}

fn parse_selector(selector: &str) -> Result<Vec<(String, bool)>, MdbaseTypeCompositionDiagnostic> {
    let invalid = || {
        error(
            "lifecycle_assignment_error",
            selector,
            "invalid lifecycle field selector",
        )
    };
    let parts = if let Some(pointer) = selector.strip_prefix('/') {
        pointer
            .split('/')
            .map(|part| {
                let mut decoded = String::new();
                let mut characters = part.chars();
                while let Some(character) = characters.next() {
                    decoded.push(match character {
                        '~' => match characters.next() {
                            Some('0') => '~',
                            Some('1') => '/',
                            _ => return Err(invalid()),
                        },
                        character => character,
                    });
                }
                Ok((decoded, false))
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        selector
            .split('.')
            .map(|part| {
                let key = part.strip_suffix("[]").unwrap_or(part);
                let mut characters = key.chars();
                if !characters
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    || !characters
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '-'))
                {
                    return Err(invalid());
                }
                Ok((key.to_string(), part.ends_with("[]")))
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    if parts.len() > 64 {
        return Err(error(
            "limit_exceeded",
            selector,
            "lifecycle field selector exceeds 64 components",
        ));
    }
    Ok(parts)
}

fn expand(
    node: Option<&Value>,
    segments: &[(String, bool)],
    path: &mut Vec<String>,
    output: &mut Vec<Vec<String>>,
    selector: &str,
) -> Result<(), MdbaseTypeCompositionDiagnostic> {
    let Some(((key, expands), rest)) = segments.split_first() else {
        if output.len() >= MAX_TARGETS {
            return Err(error(
                "limit_exceeded",
                selector,
                "too many lifecycle assignment targets",
            ));
        }
        output.push(path.clone());
        return Ok(());
    };
    let invalid = || {
        error(
            "lifecycle_assignment_error",
            selector,
            "field traverses a scalar or an invalid array index",
        )
    };
    let next = match node {
        None => None,
        Some(Value::Object(object)) => object.get(key),
        Some(Value::Array(array)) => {
            let index = key
                .parse::<usize>()
                .ok()
                .filter(|index| index.to_string() == *key)
                .ok_or_else(invalid)?;
            Some(array.get(index).ok_or_else(invalid)?)
        }
        _ => return Err(invalid()),
    };
    path.push(key.clone());
    if *expands {
        let array = next.and_then(Value::as_array).ok_or_else(invalid)?;
        for (index, value) in array.iter().enumerate() {
            path.push(index.to_string());
            expand(Some(value), rest, path, output, selector)?;
            path.pop();
        }
    } else {
        expand(next, rest, path, output, selector)?;
    }
    path.pop();
    Ok(())
}

fn set_value(node: &mut Value, path: &[String], value: Value) {
    let Some((key, rest)) = path.split_first() else {
        *node = value;
        return;
    };
    let next = match node {
        Value::Object(object) => object
            .entry(key)
            .or_insert_with(|| Value::Object(serde_json::Map::default())),
        Value::Array(array) => &mut array[key.parse::<usize>().expect("validated array index")],
        _ => unreachable!("traversal validated against original snapshot"),
    };
    set_value(next, rest, value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assignments(value: Value) -> BTreeMap<String, Value> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn nested_fields_arrays_and_escaped_pointers_preserve_other_values() {
        let draft = json!({"items": [{"id": 1}, {"id": 2}], "untouched": null});
        let result = apply_mdbase_lifecycle_assignments(
            &draft,
            &assignments(json!({
                "meta.created": "today", "items[].done": true, "/items/0/id": 3,
                "/a~1b/~0": [null, false], "/": "empty key"
            })),
        )
        .unwrap();
        assert_eq!(
            result,
            json!({"items": [{"id": 3, "done": true}, {"id": 2, "done": true}], "untouched": null, "meta": {"created": "today"}, "a/b": {"~": [null, false]}, "": "empty key"})
        );
        assert_eq!(
            draft,
            json!({"items": [{"id": 1}, {"id": 2}], "untouched": null})
        );
    }

    #[test]
    fn aliases_coalesce_but_conflicting_or_overlapping_targets_fail() {
        assert_eq!(
            apply_mdbase_lifecycle_assignments(
                &json!({}),
                &assignments(json!({"a.b": 1, "/a/b": 1}))
            )
            .unwrap(),
            json!({"a": {"b": 1}})
        );
        for values in [
            json!({"a.b": 1, "/a/b": 2}),
            json!({"a": {}, "a.b": 1}),
            json!({"items[].id": 1, "/items/0/id": 2}),
        ] {
            let error = apply_mdbase_lifecycle_assignments(
                &json!({"items": [{"id": 0}]}),
                &assignments(values),
            )
            .unwrap_err();
            assert_eq!(error.code, "type_conflict");
            assert_eq!(error.locations.len(), 2);
        }
    }

    #[test]
    fn unsafe_traversal_missing_expansions_and_invalid_selectors_fail() {
        for field in [
            "",
            "a..b",
            "a[0]",
            "/a~2b",
            "scalar.child",
            "/items/01",
            "/items/-",
            "/items/9",
            "missing[].id",
            "empty[].id",
        ] {
            let error = apply_mdbase_lifecycle_assignments(
                &json!({"scalar": null, "items": [1], "empty": []}),
                &BTreeMap::from([(field.to_string(), json!(true))]),
            )
            .unwrap_err();
            assert_eq!(error.code, "lifecycle_assignment_error", "{field}");
        }
    }

    #[test]
    fn expansion_is_bounded_before_applying_any_changes() {
        let draft = json!({"items": vec![0; MAX_TARGETS + 1]});
        let error =
            apply_mdbase_lifecycle_assignments(&draft, &assignments(json!({"items[]": true})))
                .unwrap_err();
        assert_eq!(error.code, "limit_exceeded");
        assert_eq!(draft["items"][0], 0);
    }
}
