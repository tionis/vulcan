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

/// Input request key for confirming a destructive call.
const CONFIRM_KEY: &str = "confirm:destructive";

/// What a retry's `inputResponses` answered.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InputAnswers {
    /// A note choice was declined or cancelled: report the ambiguity as an
    /// error rather than ask again.
    pub declined_note: bool,
    /// The answer to a destructive-call confirmation, if one was given:
    /// `true` only when accepted with `confirm: true`.
    pub confirmed: Option<bool>,
}

/// Take a retry's `inputResponses` and `requestState` out of `params`
/// (they are not tool-call parameters), and apply accepted note choices to
/// the arguments.
pub fn apply_input_responses(params: &mut Value) -> InputAnswers {
    let mut answers = InputAnswers::default();
    let Some(object) = params.as_object_mut() else {
        return answers;
    };
    object.remove("requestState");
    let Some(Value::Object(responses)) = object.remove("inputResponses") else {
        return answers;
    };
    for (key, response) in responses {
        if key == CONFIRM_KEY {
            let accepted = response.get("action").and_then(Value::as_str) == Some("accept")
                && response
                    .pointer("/content/confirm")
                    .and_then(Value::as_bool)
                    == Some(true);
            answers.confirmed = Some(accepted);
            continue;
        }
        let Some(argument) = key.strip_prefix(NOTE_KEY_PREFIX) else {
            continue;
        };
        if response.get("action").and_then(Value::as_str) != Some("accept") {
            answers.declined_note = true;
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
    answers
}

/// Whether a `tools/call` would overwrite or delete: a tool that needs the
/// profile's `destructive` permission, or `file_put` with `overwrite`. A dry
/// run changes nothing and is not destructive.
#[must_use]
pub fn call_is_destructive(params: Option<&Value>) -> bool {
    let Some(params) = params else {
        return false;
    };
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let arguments = params.get("arguments");
    let flag = |key: &str| {
        arguments
            .and_then(|arguments| arguments.get(key))
            .and_then(Value::as_bool)
            == Some(true)
    };
    if flag("dry_run") {
        return false;
    }
    crate::mcp_catalog::tool_by_name(name).is_some_and(|tool| tool.destructive)
        || (name == "file_put" && flag("overwrite"))
}

/// The `input_required` result asking the person to confirm a destructive
/// call, naming the tool and the target it acts on.
#[must_use]
pub fn confirmation_input(params: Option<&Value>) -> Value {
    let name = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let arguments = params.and_then(|params| params.get("arguments"));
    let target = ["path", "note", "task", "key", "id"]
        .iter()
        .find_map(|key| {
            arguments
                .and_then(|arguments| arguments.get(*key))
                .and_then(Value::as_str)
        });
    let message = match target {
        Some(target) => {
            format!("`{name}` will overwrite or delete content at `{target}`. Allow this change?")
        }
        None => format!("`{name}` will overwrite or delete vault content. Allow this change?"),
    };
    json!({
        "resultType": "input_required",
        "inputRequests": {
            CONFIRM_KEY: {
                "method": "elicitation/create",
                "params": {
                    "mode": "form",
                    "message": message,
                    "requestedSchema": {
                        "type": "object",
                        "properties": {
                            "confirm": {
                                "type": "boolean",
                                "title": "Allow",
                                "description": "Allow this overwrite or delete",
                                "default": false,
                            }
                        },
                        "required": ["confirm"],
                    },
                },
            }
        },
    })
}

/// The tool error for a destructive call the vault asks to confirm, when the
/// person declined or the client cannot ask.
#[must_use]
pub fn confirmation_refused(params: Option<&Value>, can_ask: bool) -> String {
    let name = params
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    if can_ask {
        format!("confirmation_declined: the change by `{name}` was not confirmed, so nothing was changed")
    } else {
        "confirmation_required: this vault asks for confirmation before destructive changes (`mcp.confirm_destructive`), and this client cannot show a confirmation form; nothing was changed. A dry run is still allowed."
            .to_string()
    }
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
    fn destructive_calls_are_the_destructive_tools_and_overwrites_but_not_dry_runs() {
        let call = |name: &str, arguments: Value| json!({"name": name, "arguments": arguments});
        assert!(call_is_destructive(Some(&call(
            "note_delete",
            json!({"note": "a"})
        ))));
        assert!(call_is_destructive(Some(&call(
            "note_set",
            json!({"note": "a"})
        ))));
        assert!(!call_is_destructive(Some(&call(
            "note_delete",
            json!({"note": "a", "dry_run": true})
        ))));
        assert!(!call_is_destructive(Some(&call(
            "note_append",
            json!({"note": "a"})
        ))));
        assert!(!call_is_destructive(Some(&call(
            "file_put",
            json!({"path": "a"})
        ))));
        assert!(call_is_destructive(Some(&call(
            "file_put",
            json!({"path": "a", "overwrite": true})
        ))));
        assert!(!call_is_destructive(None));
    }

    #[test]
    fn confirmation_answers_count_only_an_accepted_true() {
        let retry = |response: Value| {
            let mut params =
                json!({"name": "note_delete", "inputResponses": {"confirm:destructive": response}});
            let answers = apply_input_responses(&mut params);
            assert!(params.get("inputResponses").is_none());
            answers.confirmed
        };
        assert_eq!(
            retry(json!({"action": "accept", "content": {"confirm": true}})),
            Some(true)
        );
        assert_eq!(
            retry(json!({"action": "accept", "content": {"confirm": false}})),
            Some(false)
        );
        assert_eq!(retry(json!({"action": "decline"})), Some(false));
        let mut plain = json!({"name": "note_delete"});
        assert_eq!(apply_input_responses(&mut plain).confirmed, None);
    }

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
        assert!(!apply_input_responses(&mut params).declined_note);
        assert_eq!(
            params,
            json!({"name": "note_get", "arguments": {"note": "Projects/Alpha.md"}})
        );

        let mut declined = json!({
            "name": "note_get",
            "arguments": {"note": "Alpha"},
            "inputResponses": {"note:note": {"action": "decline"}}
        });
        assert!(apply_input_responses(&mut declined).declined_note);
        assert_eq!(declined["arguments"]["note"], "Alpha");
    }
}
