//! Input requests (2026-07-28 multi round-trip requests): when a modern
//! `tools/call` names a note ambiguously and the client declares form
//! elicitation, the answer is `input_required` with a form listing the
//! candidate notes, instead of an error. The client retries the same call
//! with `inputResponses`; the chosen path replaces the ambiguous argument.
//! The retry carries everything needed, so no request state is kept.

use std::fmt::Write as _;

use serde_json::{json, Map, Value};
use vulcan_core::GraphQueryError;

use crate::mcp_authority::McpAuthority;

/// Prefix of the input request key for a note argument: `note:<argument>`.
const NOTE_KEY_PREFIX: &str = "note:";
/// Candidates offered in one form.
const MAX_CHOICES: usize = 25;

/// Whether a request's `_meta` declares form-mode elicitation (an empty
/// `elicitation` object means form).
#[must_use]
pub fn declares_form_elicitation(params: Option<&Value>) -> bool {
    params
        .and_then(|params| params.pointer("/_meta/io.modelcontextprotocol~1clientCapabilities"))
        .and_then(|capabilities| capabilities.get("elicitation"))
        .and_then(Value::as_object)
        .is_some_and(|elicitation| {
            elicitation.is_empty() || elicitation.get("form").is_some_and(Value::is_object)
        })
}

/// Take a retry's `inputResponses` and `requestState` out of `params`
/// (they are not tool-call parameters), and apply accepted note choices to
/// the arguments. Returns whether the client declined or cancelled a choice:
/// it is then given the ambiguity as an error rather than asked again.
pub fn apply_input_responses(params: &mut Value) -> bool {
    let Some(object) = params.as_object_mut() else {
        return false;
    };
    object.remove("requestState");
    let Some(Value::Object(responses)) = object.remove("inputResponses") else {
        return false;
    };
    let mut declined = false;
    for (key, response) in responses {
        let Some(argument) = key.strip_prefix(NOTE_KEY_PREFIX) else {
            continue;
        };
        if response.get("action").and_then(Value::as_str) != Some("accept") {
            declined = true;
            continue;
        }
        let Some(choice) = response
            .pointer("/content/note")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        if let Some(Value::Object(arguments)) = object.get_mut("arguments") {
            if arguments.contains_key(argument) {
                arguments.insert(argument.to_string(), Value::String(choice));
            }
        }
    }
    declined
}

/// The `input_required` result for a tool error that names an ambiguous
/// note, when the ambiguous identifier is one of the call's arguments.
#[must_use]
pub fn ambiguity_input(
    authority: &McpAuthority,
    message: &str,
    arguments: Option<&Map<String, Value>>,
) -> Option<Value> {
    let identifier = message
        .strip_prefix("note identifier '")?
        .rsplit_once("' is ambiguous")?
        .0;
    let argument = arguments?
        .iter()
        .find(|(_, value)| value.as_str() == Some(identifier))?
        .0;
    // Resolve again under the caller's permissions for the exact candidate
    // paths: the message joins them with commas, which paths may contain.
    let Err(GraphQueryError::AmbiguousIdentifier { matches, .. }) =
        vulcan_core::graph::resolve_note_reference_with_guard(
            authority.paths(),
            identifier,
            authority.guard(),
        )
    else {
        return None;
    };
    if matches.len() < 2 {
        return None;
    }
    let choices = matches
        .iter()
        .take(MAX_CHOICES)
        .map(|path| json!({ "const": path, "title": path }))
        .collect::<Vec<_>>();
    let more = matches.len().saturating_sub(MAX_CHOICES);
    let mut prompt = format!("`{identifier}` matches more than one note. Which one did you mean?");
    if more > 0 {
        let _ = write!(
            prompt,
            " ({more} more not shown; give a longer path to narrow it down.)"
        );
    }
    Some(json!({
        "resultType": "input_required",
        "inputRequests": {
            format!("{NOTE_KEY_PREFIX}{argument}"): {
                "method": "elicitation/create",
                "params": {
                    "mode": "form",
                    "message": prompt,
                    "requestedSchema": {
                        "type": "object",
                        "properties": {
                            "note": {
                                "type": "string",
                                "title": "Note",
                                "oneOf": choices,
                            }
                        },
                        "required": ["note"],
                    },
                },
            }
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_elicitation_is_declared_by_an_empty_or_form_object() {
        let with = |elicitation: Value| json!({"_meta": {"io.modelcontextprotocol/clientCapabilities": {"elicitation": elicitation}}});
        assert!(declares_form_elicitation(Some(&with(json!({})))));
        assert!(declares_form_elicitation(Some(&with(json!({"form": {}})))));
        assert!(!declares_form_elicitation(Some(&with(json!({"url": {}})))));
        assert!(!declares_form_elicitation(Some(&json!({"_meta": {}}))));
    }

    #[test]
    fn accepted_choices_replace_the_argument_and_refusals_are_reported() {
        let mut params = json!({
            "name": "note_get",
            "arguments": {"note": "Alpha"},
            "inputResponses": {
                "note:note": {"action": "accept", "content": {"note": "Projects/Alpha.md"}},
                "note:missing": {"action": "accept", "content": {"note": "x.md"}},
                "other": {"action": "accept"}
            },
            "requestState": "ignored"
        });
        assert!(!apply_input_responses(&mut params));
        assert_eq!(
            params,
            json!({"name": "note_get", "arguments": {"note": "Projects/Alpha.md"}})
        );

        let mut declined = json!({
            "name": "note_get",
            "arguments": {"note": "Alpha"},
            "inputResponses": {"note:note": {"action": "decline"}}
        });
        assert!(apply_input_responses(&mut declined));
        assert_eq!(declined["arguments"]["note"], "Alpha");
    }
}
