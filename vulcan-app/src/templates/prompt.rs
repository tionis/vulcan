//! Answers for `tp.system.prompt()`, `suggester()`, and `multi_suggester()`.
//!
//! An answer comes from a `--var` binding first, then from an interactive
//! prompter the host installed for the current thread, and finally from the
//! call's default. Without any of those the render fails with an error that
//! names the exact `--var` key to pass, so automation never silently records
//! a guessed answer.

use super::{lookup_template_var, prompt_lookup_key, slugify_var_key, PromptLookupKey};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Error code for a prompt nothing could answer.
pub const TEMPLATE_INPUT_REQUIRED_CODE: &str = "template_input_required";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplatePromptRequest {
    pub message: String,
    pub default: Option<String>,
    pub multiline: bool,
    /// The `--var` key that would answer this prompt non-interactively.
    pub var_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSuggestRequest {
    pub placeholder: String,
    pub options: Vec<String>,
    pub multiple: bool,
    pub limit: Option<usize>,
    /// The `--var` key that would answer this choice non-interactively.
    pub var_key: String,
}

/// Interactive answers for template prompts, installed by a host that owns
/// a terminal. `Ok(None)` means the user cancelled.
pub trait TemplatePrompter {
    /// # Errors
    /// Returns an error when the prompt cannot be shown or read.
    fn prompt(&self, request: &TemplatePromptRequest) -> Result<Option<String>, String>;

    /// Returns indices into `request.options`.
    ///
    /// # Errors
    /// Returns an error when the choice cannot be shown or read.
    fn suggest(&self, request: &TemplateSuggestRequest) -> Result<Option<Vec<usize>>, String>;
}

thread_local! {
    static PROMPTER: RefCell<Option<Rc<dyn TemplatePrompter>>> = const { RefCell::new(None) };
}

/// Runs `run` with `prompter` answering template prompts on this thread.
pub fn with_template_prompter<R>(prompter: Rc<dyn TemplatePrompter>, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<Rc<dyn TemplatePrompter>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            PROMPTER.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let previous = PROMPTER.with(|slot| slot.borrow_mut().replace(prompter));
    let _restore = Restore(previous);
    run()
}

fn current_prompter() -> Option<Rc<dyn TemplatePrompter>> {
    PROMPTER.with(|slot| slot.borrow().clone())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PromptError {
    /// Nothing could answer; the message names the `--var` to pass.
    InputRequired(String),
    Failed(String),
}

impl PromptError {
    #[cfg(any(test, feature = "js_runtime"))]
    pub(super) fn message(&self) -> &str {
        match self {
            Self::InputRequired(message) | Self::Failed(message) => message,
        }
    }
}

pub(super) struct PromptCall<'a> {
    pub(super) message: &'a str,
    pub(super) default: Option<&'a str>,
    pub(super) throw_on_cancel: bool,
    pub(super) multiline: bool,
    pub(super) ordinal: usize,
}

/// Resolves `tp.system.prompt()`; `None` is Templater's cancelled result.
pub(super) fn resolve_prompt(
    vars: &HashMap<String, String>,
    call: &PromptCall<'_>,
) -> Result<Option<String>, PromptError> {
    let key = prompt_lookup_key(call.message, call.ordinal);
    if let Some(value) = lookup_template_var(vars, &key, Some(call.message)) {
        return Ok(Some(value));
    }
    if let Some(prompter) = current_prompter() {
        let answer = prompter
            .prompt(&TemplatePromptRequest {
                message: call.message.to_string(),
                default: call.default.map(ToOwned::to_owned),
                multiline: call.multiline,
                var_key: key.slug.clone(),
            })
            .map_err(PromptError::Failed)?;
        return match answer {
            Some(answer) => Ok(Some(answer)),
            None if call.throw_on_cancel => Err(PromptError::Failed(format!(
                "{} was cancelled",
                describe_call("tp.system.prompt", call.message)
            ))),
            None => Ok(None),
        };
    }
    if let Some(default) = call.default {
        return Ok(Some(default.to_string()));
    }
    Err(PromptError::InputRequired(format!(
        "{} needs an answer: pass --var {}=<value>, or run in an interactive terminal",
        describe_call("tp.system.prompt", call.message),
        var_flag_key(&key.slug),
    )))
}

pub(super) struct SuggestCall<'a> {
    pub(super) options: &'a [String],
    /// Display strings of the items themselves, matched as alternatives.
    pub(super) item_keys: &'a [String],
    pub(super) throw_on_cancel: bool,
    pub(super) placeholder: &'a str,
    pub(super) limit: Option<usize>,
    pub(super) multiple: bool,
    pub(super) ordinal: usize,
}

/// Resolves a suggester to option indices; `None` is Templater's cancelled
/// result. Single-choice suggesters return exactly one index.
pub(super) fn resolve_suggestion(
    vars: &HashMap<String, String>,
    call: &SuggestCall<'_>,
) -> Result<Option<Vec<usize>>, PromptError> {
    let function = if call.multiple {
        "tp.system.multi_suggester"
    } else {
        "tp.system.suggester"
    };
    let key = suggester_lookup_key(call.placeholder, call.ordinal, call.multiple);
    let bound = lookup_template_var(vars, &key, Some(call.placeholder).filter(|p| !p.is_empty()))
        .or_else(|| vars.get(legacy_suggester_key(call.multiple)).cloned());
    if let Some(value) = bound {
        return match_bound_choice(&value, call)
            .map(Some)
            .map_err(|unmatched| {
                PromptError::Failed(format!(
                    "--var {}={unmatched:?} matches none of the options of {}; choose one of: {}",
                    var_flag_key(&key.slug),
                    describe_call(function, call.placeholder),
                    list_options(call.options),
                ))
            });
    }
    if call.options.is_empty() {
        return Ok(call.multiple.then(Vec::new));
    }
    if let Some(prompter) = current_prompter() {
        let answer = prompter
            .suggest(&TemplateSuggestRequest {
                placeholder: call.placeholder.to_string(),
                options: call.options.to_vec(),
                multiple: call.multiple,
                limit: call.limit,
                var_key: key.slug.clone(),
            })
            .map_err(PromptError::Failed)?;
        return match answer {
            Some(indices) if indices.iter().all(|index| *index < call.options.len()) => {
                Ok(Some(indices))
            }
            Some(_) => Err(PromptError::Failed(format!(
                "{} received an out-of-range choice",
                describe_call(function, call.placeholder)
            ))),
            None if call.throw_on_cancel => Err(PromptError::Failed(format!(
                "{} was cancelled",
                describe_call(function, call.placeholder)
            ))),
            None => Ok(None),
        };
    }
    Err(PromptError::InputRequired(format!(
        "{} needs a choice: pass --var {}=<option>{}, or run in an interactive terminal; options: {}",
        describe_call(function, call.placeholder),
        var_flag_key(&key.slug),
        if call.multiple { "[,<option>…]" } else { "" },
        list_options(call.options),
    )))
}

/// Asks for a `QuickAdd` `{{VALUE:name}}` that no `--var` answered: a choice
/// list becomes a suggester, anything else a prompt with its default.
pub(super) fn resolve_quickadd_value(
    name: &str,
    choices: &[String],
    default: Option<&str>,
) -> Result<String, PromptError> {
    let key = super::slugify_var_key(name);
    let label = format!("{{{{VALUE:{name}}}}}");
    if let Some(prompter) = current_prompter() {
        if choices.is_empty() {
            let answer = prompter
                .prompt(&TemplatePromptRequest {
                    message: name.to_string(),
                    default: default.map(ToOwned::to_owned),
                    multiline: false,
                    var_key: key,
                })
                .map_err(PromptError::Failed)?;
            return answer.ok_or_else(|| PromptError::Failed(format!("{label} was cancelled")));
        }
        let answer = prompter
            .suggest(&TemplateSuggestRequest {
                placeholder: name.to_string(),
                options: choices.to_vec(),
                multiple: false,
                limit: None,
                var_key: key,
            })
            .map_err(PromptError::Failed)?;
        return answer
            .and_then(|indices| {
                indices
                    .first()
                    .and_then(|index| choices.get(*index))
                    .cloned()
            })
            .ok_or_else(|| PromptError::Failed(format!("{label} was cancelled")));
    }
    if let Some(default) = default {
        return Ok(default.to_string());
    }
    Err(PromptError::InputRequired(if choices.is_empty() {
        format!(
            "{label} needs an answer: pass --var {}=<value>, or run in an interactive terminal",
            var_flag_key(&key)
        )
    } else {
        format!(
            "{label} needs a choice: pass --var {}=<option>, or run in an interactive terminal; options: {}",
            var_flag_key(&key),
            list_options(choices)
        )
    }))
}

/// Asks for a free-text `QuickAdd` value (such as a `{{VDATE}}` date) that
/// no `--var` answered.
pub(super) fn resolve_prompt_text(
    label: &str,
    name: &str,
    default: Option<&str>,
    placeholder: &str,
) -> Result<String, PromptError> {
    let key = super::slugify_var_key(name);
    if let Some(prompter) = current_prompter() {
        return prompter
            .prompt(&TemplatePromptRequest {
                message: name.to_string(),
                default: default.map(ToOwned::to_owned),
                multiline: false,
                var_key: key,
            })
            .map_err(PromptError::Failed)?
            .ok_or_else(|| PromptError::Failed(format!("{label} was cancelled")));
    }
    default.map(ToOwned::to_owned).ok_or_else(|| {
        PromptError::InputRequired(format!(
            "{label} needs an answer: pass --var {}={placeholder}, or run in an interactive terminal",
            var_flag_key(&key)
        ))
    })
}

fn suggester_lookup_key(placeholder: &str, ordinal: usize, multiple: bool) -> PromptLookupKey {
    let prefix = if multiple {
        "multi_suggester"
    } else {
        "suggester"
    };
    PromptLookupKey {
        raw: placeholder.to_string(),
        slug: if placeholder.trim().is_empty() || slugify_var_key(placeholder).is_empty() {
            format!("{prefix}_{ordinal}")
        } else {
            slugify_var_key(placeholder)
        },
    }
}

/// Before ordinal keys existed every suggester read `--var suggester=…`.
fn legacy_suggester_key(multiple: bool) -> &'static str {
    if multiple {
        "multi_suggester"
    } else {
        "suggester"
    }
}

/// Matches a `--var` value against option labels, then the items
/// themselves, then case-insensitively, then as a 1-based option number.
/// Multi-choice values may be a JSON array or a comma-separated list.
fn match_bound_choice(value: &str, call: &SuggestCall<'_>) -> Result<Vec<usize>, String> {
    let wanted: Vec<String> = if call.multiple {
        serde_json::from_str::<Vec<String>>(value).unwrap_or_else(|_| {
            if find_choice(value, call).is_some() {
                vec![value.to_string()]
            } else {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(ToOwned::to_owned)
                    .collect()
            }
        })
    } else {
        vec![value.to_string()]
    };
    let mut indices = Vec::with_capacity(wanted.len());
    for choice in &wanted {
        let index = find_choice(choice, call).ok_or_else(|| choice.clone())?;
        if !indices.contains(&index) {
            indices.push(index);
        }
    }
    Ok(indices)
}

fn find_choice(choice: &str, call: &SuggestCall<'_>) -> Option<usize> {
    let position = |values: &[String]| values.iter().position(|value| value == choice);
    position(call.options)
        .or_else(|| position(call.item_keys))
        .or_else(|| {
            call.options
                .iter()
                .position(|option| option.eq_ignore_ascii_case(choice))
        })
        .or_else(|| {
            choice
                .parse::<usize>()
                .ok()
                .and_then(|number| number.checked_sub(1))
                .filter(|index| *index < call.options.len())
        })
}

fn describe_call(function: &str, label: &str) -> String {
    if label.trim().is_empty() {
        format!("{function}()")
    } else {
        format!("{function}({label:?})")
    }
}

/// Quotes a `--var` key for a shell when it needs it.
fn var_flag_key(key: &str) -> String {
    if key
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        key.to_string()
    } else {
        format!("'{key}'")
    }
}

fn list_options(options: &[String]) -> String {
    const MAX_LISTED: usize = 20;
    let mut listed = options
        .iter()
        .take(MAX_LISTED)
        .map(|option| format!("{option:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    if options.len() > MAX_LISTED {
        listed = format!("{listed}, … ({} more)", options.len() - MAX_LISTED);
    }
    listed
}

/// A prompt a template asks, found by reading its source, so `--var` keys
/// can be supplied before rendering.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TemplateInput {
    /// `tp.system.prompt`, `tp.system.suggester`, or `tp.system.multi_suggester`.
    pub function: String,
    /// The prompt text or suggester placeholder, when written as a literal.
    pub label: Option<String>,
    /// The `--var` key that answers it.
    pub var_key: String,
    /// The literal default answer of a prompt, if any.
    pub default: Option<String>,
}

/// Best-effort list of the prompts in `source`, in source order. Prompts
/// inside loops, or whose labels are computed, may resolve differently at
/// render time; the render error then names the exact key.
pub(super) fn discover_template_inputs(source: &str) -> Vec<TemplateInput> {
    const FUNCTIONS: [&str; 3] = [
        "tp.system.multi_suggester",
        "tp.system.suggester",
        "tp.system.prompt",
    ];
    let mut inputs = Vec::new();
    let (mut prompts, mut suggesters) = (0_usize, 0_usize);
    let mut rest = source;
    while let Some((offset, function)) = FUNCTIONS
        .iter()
        .filter_map(|function| rest.find(function).map(|offset| (offset, *function)))
        .min_by_key(|(offset, function)| (*offset, std::cmp::Reverse(function.len())))
    {
        let after = &rest[offset + function.len()..];
        rest = after;
        let Some(arguments) = after.trim_start().strip_prefix('(') else {
            continue;
        };
        let args = split_call_arguments(arguments);
        let literal = |index: usize| args.get(index).and_then(|arg| string_literal(arg));
        let (label, default, var_key) = if function == "tp.system.prompt" {
            prompts += 1;
            let label = literal(0);
            let key = prompt_lookup_key(label.as_deref().unwrap_or_default(), prompts).slug;
            (label, literal(1), key)
        } else {
            suggesters += 1;
            let multiple = function == "tp.system.multi_suggester";
            let label = literal(3);
            let key =
                suggester_lookup_key(label.as_deref().unwrap_or_default(), suggesters, multiple)
                    .slug;
            (label, None, key)
        };
        inputs.push(TemplateInput {
            function: function.to_string(),
            label,
            var_key,
            default,
        });
    }
    inputs
}

/// Top-level arguments of a call whose text starts just after `(`.
fn split_call_arguments(text: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0_usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' | '`' => quote = Some(character),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth > 0 => depth -= 1,
            ')' => {
                args.push(text[start..index].trim());
                return args.into_iter().filter(|arg| !arg.is_empty()).collect();
            }
            ',' if depth == 0 => {
                args.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    Vec::new()
}

/// The value of a plain string literal argument.
fn string_literal(arg: &str) -> Option<String> {
    let quote = arg
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let inner = arg.strip_prefix(quote)?.strip_suffix(quote)?;
    if quote == '`' && inner.contains("${") {
        return None;
    }
    let mut value = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            value.extend(characters.next());
        } else {
            value.push(character);
        }
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted {
        prompt: Option<String>,
        suggest: Option<Vec<usize>>,
        seen: RefCell<Vec<String>>,
    }

    impl TemplatePrompter for Scripted {
        fn prompt(&self, request: &TemplatePromptRequest) -> Result<Option<String>, String> {
            self.seen.borrow_mut().push(request.var_key.clone());
            Ok(self.prompt.clone())
        }

        fn suggest(&self, request: &TemplateSuggestRequest) -> Result<Option<Vec<usize>>, String> {
            self.seen.borrow_mut().push(request.var_key.clone());
            Ok(self.suggest.clone())
        }
    }

    fn options() -> Vec<String> {
        vec!["Meeting".to_string(), "Idea".to_string()]
    }

    fn suggest_call<'a>(
        options: &'a [String],
        keys: &'a [String],
        multiple: bool,
    ) -> SuggestCall<'a> {
        SuggestCall {
            options,
            item_keys: keys,
            throw_on_cancel: false,
            placeholder: "Note type",
            limit: None,
            multiple,
            ordinal: 1,
        }
    }

    fn prompt_call(default: Option<&str>) -> PromptCall<'_> {
        PromptCall {
            message: "Project name",
            default,
            throw_on_cancel: false,
            multiline: false,
            ordinal: 1,
        }
    }

    #[test]
    fn discovers_prompt_inputs_and_their_var_keys() {
        let inputs = discover_template_inputs(
            "<%* const a = await tp.system.prompt(\"Project name\", 'x, y'); %>\n<% tp.system.suggester(['A', 'B'], [1, 2], false, \"Note type\") %><% tp.system.multi_suggester((t) => t, list) %><% tp.system.prompt(label) %>",
        );
        let summary: Vec<(String, Option<String>, String, Option<String>)> = inputs
            .into_iter()
            .map(|input| (input.function, input.label, input.var_key, input.default))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "tp.system.prompt".to_string(),
                    Some("Project name".to_string()),
                    "project_name".to_string(),
                    Some("x, y".to_string())
                ),
                (
                    "tp.system.suggester".to_string(),
                    Some("Note type".to_string()),
                    "note_type".to_string(),
                    None
                ),
                (
                    "tp.system.multi_suggester".to_string(),
                    None,
                    "multi_suggester_2".to_string(),
                    None
                ),
                (
                    "tp.system.prompt".to_string(),
                    None,
                    "prompt_2".to_string(),
                    None
                ),
            ]
        );
    }

    #[test]
    fn prompt_prefers_vars_then_default_then_requires_input() {
        let vars = HashMap::from([("project_name".to_string(), "Vulcan".to_string())]);
        assert_eq!(
            resolve_prompt(&vars, &prompt_call(None)).unwrap(),
            Some("Vulcan".to_string())
        );
        assert_eq!(
            resolve_prompt(&HashMap::new(), &prompt_call(Some("Default"))).unwrap(),
            Some("Default".to_string())
        );
        assert_eq!(
            resolve_prompt(&HashMap::new(), &prompt_call(None)).unwrap_err(),
            PromptError::InputRequired(
                "tp.system.prompt(\"Project name\") needs an answer: pass --var project_name=<value>, or run in an interactive terminal".to_string()
            )
        );
    }

    #[test]
    fn interactive_prompter_answers_and_cancels() {
        let prompter = Rc::new(Scripted {
            prompt: None,
            suggest: Some(vec![1]),
            seen: RefCell::new(Vec::new()),
        });
        let options = options();
        let (prompt, choice) = with_template_prompter(prompter.clone(), || {
            (
                resolve_prompt(&HashMap::new(), &prompt_call(Some("Default"))),
                resolve_suggestion(&HashMap::new(), &suggest_call(&options, &options, false)),
            )
        });
        assert_eq!(prompt.unwrap(), None);
        assert_eq!(choice.unwrap(), Some(vec![1]));
        assert_eq!(*prompter.seen.borrow(), ["project_name", "note_type"]);
        assert!(current_prompter().is_none());
    }

    #[test]
    fn bound_choices_match_labels_items_case_and_numbers() {
        let options = options();
        let keys = vec!["meeting".to_string(), "idea".to_string()];
        for (value, expected) in [("Idea", 1), ("meeting", 0), ("IDEA", 1), ("2", 1)] {
            let vars = HashMap::from([("note_type".to_string(), value.to_string())]);
            assert_eq!(
                resolve_suggestion(&vars, &suggest_call(&options, &keys, false)).unwrap(),
                Some(vec![expected]),
                "{value}"
            );
        }
        let vars = HashMap::from([("note_type".to_string(), "Task".to_string())]);
        assert_eq!(
            resolve_suggestion(&vars, &suggest_call(&options, &keys, false))
                .unwrap_err()
                .message(),
            "--var note_type=\"Task\" matches none of the options of tp.system.suggester(\"Note type\"); choose one of: \"Meeting\", \"Idea\""
        );
    }

    #[test]
    fn multi_choices_accept_lists_and_json_arrays() {
        let options = options();
        for value in ["Idea, Meeting", "[\"Idea\",\"Meeting\"]"] {
            let vars = HashMap::from([("note_type".to_string(), value.to_string())]);
            assert_eq!(
                resolve_suggestion(&vars, &suggest_call(&options, &options, true)).unwrap(),
                Some(vec![1, 0])
            );
        }
    }

    #[test]
    fn unanswered_suggesters_name_their_var_and_options() {
        let options = options();
        let mut call = suggest_call(&options, &options, false);
        call.placeholder = "";
        call.ordinal = 2;
        assert_eq!(
            resolve_suggestion(&HashMap::new(), &call).unwrap_err(),
            PromptError::InputRequired(
                "tp.system.suggester() needs a choice: pass --var suggester_2=<option>, or run in an interactive terminal; options: \"Meeting\", \"Idea\"".to_string()
            )
        );
        let vars = HashMap::from([("suggester".to_string(), "Idea".to_string())]);
        assert_eq!(resolve_suggestion(&vars, &call).unwrap(), Some(vec![1]));
    }
}
