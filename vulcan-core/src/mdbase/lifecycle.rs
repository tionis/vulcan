//! Pure mutation-time value providers. The write planner owns the clock and
//! entropy, evaluates each accepted assignment once, and retains the result in
//! its immutable preview. Applying a preview must never call these providers.

use super::{resolve_match_field, MdbaseCelClock};
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseLifecycleProviderError {
    pub code: String,
    pub message: String,
}

impl MdbaseLifecycleProviderError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            code: "lifecycle_provider_error".to_string(),
            message: message.into(),
        }
    }
}

impl Display for MdbaseLifecycleProviderError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MdbaseLifecycleProviderError {}

/// Evaluate one standard provider against persisted draft fields, not read
/// defaults. This function performs no I/O and does not mutate the draft.
///
/// `now` uses UTC RFC 3339 with millisecond precision; `today` uses the supplied
/// clock's collection timezone. UUIDs are version 4; ULIDs use the same clock's
/// Unix milliseconds and 80 bits of supplied entropy. The caller must supply
/// fresh random bytes for each ID assignment and retain the returned values.
///
/// Field references share the collection's dotted/JSON-pointer resolver. Copy
/// preserves nulls and JSON types. Missing or multiple source values are errors.
/// Slugs lowercase Unicode letters/numbers and collapse other runs to hyphens,
/// trimming boundary hyphens; non-string inputs are errors.
pub fn evaluate_mdbase_lifecycle_provider(
    provider: &Value,
    draft: &Value,
    clock: &MdbaseCelClock,
    mut entropy: impl FnMut() -> Result<[u8; 16], MdbaseLifecycleProviderError>,
) -> Result<Value, MdbaseLifecycleProviderError> {
    let object = provider
        .as_object()
        .filter(|object| object.len() == 1)
        .ok_or_else(|| {
            MdbaseLifecycleProviderError::new("expected exactly one lifecycle provider")
        })?;
    let (name, argument) = object.iter().next().expect("one provider");
    match name.as_str() {
        "now" | "today" | "uuid" | "ulid" if argument != &Value::Bool(true) => Err(
            MdbaseLifecycleProviderError::new(format!("{name} requires true")),
        ),
        "now" => Ok(Value::String(
            clock.now_utc().to_rfc3339_opts(SecondsFormat::Millis, true),
        )),
        "today" => Ok(Value::String(clock.today())),
        "uuid" => {
            let mut bytes = entropy()?;
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            let hex = format!("{:032x}", u128::from_be_bytes(bytes));
            Ok(Value::String(format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            )))
        }
        "ulid" => {
            let millis = u64::try_from(clock.now_utc().timestamp_millis())
                .ok()
                .filter(|millis| *millis < (1_u64 << 48))
                .ok_or_else(|| MdbaseLifecycleProviderError::new("clock is outside ULID range"))?;
            let random = u128::from_be_bytes(entropy()?) & ((1_u128 << 80) - 1);
            Ok(Value::String(
                ulid::Ulid::from_parts(millis, random).to_string(),
            ))
        }
        "literal" => Ok(argument.clone()),
        "copy" | "slugify" => {
            let selector = argument
                .as_str()
                .filter(|selector| !selector.is_empty())
                .ok_or_else(|| {
                    MdbaseLifecycleProviderError::new("expected a source field reference")
                })?;
            let fields = draft.as_object().ok_or_else(|| {
                MdbaseLifecycleProviderError::new("lifecycle draft must be an object")
            })?;
            let resolved = resolve_match_field(fields, selector);
            let value = match resolved.values.as_slice() {
                [value] if resolved.exists => *value,
                _ => {
                    return Err(MdbaseLifecycleProviderError::new(format!(
                        "source field {selector} must resolve to exactly one value"
                    )))
                }
            };
            if name == "copy" {
                return Ok(value.clone());
            }
            let text = value.as_str().ok_or_else(|| {
                MdbaseLifecycleProviderError::new("slugify source must be a string")
            })?;
            Ok(Value::String(slugify(text)))
        }
        _ => Err(MdbaseLifecycleProviderError::new(format!(
            "unsupported lifecycle provider: {name}"
        ))),
    }
}

fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if separator && !slug.is_empty() {
                slug.push('-');
            }
            slug.push(character);
            separator = false;
        } else {
            separator = true;
        }
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn clock() -> MdbaseCelClock {
        MdbaseCelClock::new(
            "2026-09-08T23:30:45.123456Z".parse().unwrap(),
            "Europe/Berlin",
        )
        .unwrap()
    }

    #[allow(clippy::needless_pass_by_value)] // Keep JSON fixtures concise.
    fn evaluate(provider: Value, draft: Value) -> Result<Value, MdbaseLifecycleProviderError> {
        evaluate_mdbase_lifecycle_provider(&provider, &draft, &clock(), || {
            panic!("non-ID providers must not request entropy")
        })
    }

    #[test]
    fn fixed_clock_preserves_precision_and_collection_date() {
        assert_eq!(
            evaluate(json!({"now": true}), json!({})).unwrap(),
            "2026-09-08T23:30:45.123Z"
        );
        assert_eq!(
            evaluate(json!({"today": true}), json!({})).unwrap(),
            "2026-09-09"
        );
    }

    #[test]
    fn ids_use_supplied_entropy_and_correct_wire_formats() {
        let mut calls = 0;
        let mut entropy = || {
            calls += 1;
            Ok([0xff; 16])
        };
        let random_uuid = evaluate_mdbase_lifecycle_provider(
            &json!({"uuid": true}),
            &json!({}),
            &clock(),
            &mut entropy,
        )
        .unwrap();
        let sortable_id = evaluate_mdbase_lifecycle_provider(
            &json!({"ulid": true}),
            &json!({}),
            &clock(),
            &mut entropy,
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(random_uuid, "ffffffff-ffff-4fff-bfff-ffffffffffff");
        let parsed: ulid::Ulid = sortable_id.as_str().unwrap().parse().unwrap();
        assert_eq!(
            parsed.timestamp_ms(),
            u64::try_from(clock().now_utc().timestamp_millis()).unwrap()
        );
        assert_eq!(parsed.random(), (1_u128 << 80) - 1);
        assert_eq!(sortable_id.as_str().unwrap(), parsed.to_string());
    }

    #[test]
    fn copy_and_literal_preserve_types_and_explicit_null() {
        let draft = json!({"nested": {"value": [null, 1, true]}, "a/b": null});
        assert_eq!(
            evaluate(json!({"copy": "nested.value"}), draft.clone()).unwrap(),
            json!([null, 1, true])
        );
        assert_eq!(
            evaluate(json!({"copy": "/a~1b"}), draft.clone()).unwrap(),
            Value::Null
        );
        assert!(evaluate(json!({"copy": "missing"}), draft).is_err());
        for value in [
            Value::Null,
            json!(false),
            json!(3),
            json!([1]),
            json!({"now": true}),
        ] {
            assert_eq!(
                evaluate(json!({"literal": value}), json!({})).unwrap(),
                value
            );
        }
    }

    #[test]
    fn slugify_handles_boundaries_unicode_and_empty_values() {
        for (input, expected) in [
            ("Created task", "created-task"),
            ("  Hello___WORLD! ", "hello-world"),
            ("Été 東京 42", "été-東京-42"),
            ("---", ""),
            ("", ""),
        ] {
            assert_eq!(
                evaluate(json!({"slugify": "title"}), json!({"title": input})).unwrap(),
                expected
            );
        }
        assert!(evaluate(json!({"slugify": "title"}), json!({"title": 42})).is_err());
        assert!(evaluate(
            json!({"copy": "items[].id"}),
            json!({"items": [{"id": 1}, {"id": 2}]})
        )
        .is_err());
    }

    #[test]
    fn malformed_or_unknown_providers_fail_without_generation() {
        for provider in [
            json!(null),
            json!("now"),
            json!({}),
            json!({"now": true, "uuid": true}),
            json!({"now": false}),
            json!({"uuid": 1}),
            json!({"copy": ""}),
            json!({"shell": "echo hi"}),
        ] {
            assert_eq!(
                evaluate(provider, json!({})).unwrap_err().code,
                "lifecycle_provider_error"
            );
        }
    }

    #[test]
    fn entropy_failures_and_unrepresentable_ulid_clocks_fail_closed() {
        let error = evaluate_mdbase_lifecycle_provider(
            &json!({"uuid": true}),
            &json!({}),
            &clock(),
            || Err(MdbaseLifecycleProviderError::new("entropy unavailable")),
        )
        .unwrap_err();
        assert_eq!(error.message, "entropy unavailable");
        let clock = MdbaseCelClock::new("1969-12-31T23:59:59Z".parse().unwrap(), "UTC").unwrap();
        assert!(evaluate_mdbase_lifecycle_provider(
            &json!({"ulid": true}),
            &json!({}),
            &clock,
            || panic!("invalid clock must fail before generation")
        )
        .is_err());
    }
}
