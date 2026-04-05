use crate::{render_dataview_inline_value, CliError, OutputFormat};
use rustyline::completion::{Completer, Pair};
use rustyline::config::{CompletionType, Config};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::time::Duration;
use vulcan_core::{
    DataviewJsEvalOptions, DataviewJsOutput, DataviewJsResult, DataviewJsSession, JsRuntimeSandbox,
    VaultPaths,
};

const PRIMARY_PROMPT: &str = "vulcan> ";
const CONTINUATION_PROMPT: &str = "......> ";
const MAX_HISTORY_ENTRIES: usize = 200;
const REPL_COMPLETIONS: &[&str] = &[
    ".exit",
    ".quit",
    "console.log(",
    "help(",
    "vault.",
    "vault.note(",
    "vault.notes(",
    "vault.query(",
    "vault.search(",
    "vault.events(",
    "vault.graph.",
    "vault.graph.shortestPath(",
    "vault.graph.components(",
    "vault.graph.hubs(",
    "vault.graph.deadEnds(",
    "vault.graph.neighbors(",
    "vault.graph.subgraph(",
    "vault.graph.filter(",
    "vault.daily.",
    "vault.daily.today(",
    "vault.daily.get(",
    "vault.daily.range(",
    "vault.daily.append(",
    "vault.set(",
    "vault.create(",
    "vault.append(",
    "vault.patch(",
    "vault.update(",
    "vault.unset(",
    "vault.inbox(",
    "vault.transaction(",
    "vault.refactor.",
    "vault.refactor.renameAlias(",
    "vault.refactor.renameHeading(",
    "vault.refactor.renameBlockRef(",
    "vault.refactor.renameProperty(",
    "vault.refactor.mergeTags(",
    "vault.refactor.move(",
    "web.search(",
    "web.fetch(",
];

pub(crate) fn run_js_repl(
    paths: &VaultPaths,
    output: OutputFormat,
    timeout: Option<Duration>,
    sandbox: Option<JsRuntimeSandbox>,
) -> Result<(), CliError> {
    let session = DataviewJsSession::new(paths, None, DataviewJsEvalOptions { timeout, sandbox })
        .map_err(CliError::operation)?;
    fs::create_dir_all(paths.vulcan_dir()).map_err(CliError::operation)?;

    let history_path = paths.vulcan_dir().join("repl_history");
    let mut history = load_history(&history_path)?;
    let mut editor = build_repl_editor(&history, REPL_COMPLETIONS)?;
    let mut pending = String::new();

    loop {
        let prompt = if pending.is_empty() {
            PRIMARY_PROMPT
        } else {
            CONTINUATION_PROMPT
        };
        match editor.readline(prompt) {
            Ok(line) => {
                if !pending.is_empty() {
                    pending.push('\n');
                }
                pending.push_str(&line);
                if repl_input_needs_continuation(&pending) {
                    continue;
                }

                let source = std::mem::take(&mut pending);
                let trimmed = source.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if record_submission(&mut history, &source) {
                    let _ = editor
                        .add_history_entry(source.as_str())
                        .map_err(|error| CliError::operation(&error))?;
                }
                if matches!(trimmed, ".exit" | ".quit") {
                    break;
                }

                match session.evaluate(&source).map_err(CliError::operation) {
                    Ok(result) => print_repl_result(output, &result)?,
                    Err(error) => print_repl_error(output, &error.to_string())?,
                }
            }
            Err(ReadlineError::Interrupted) => {
                pending.clear();
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(error) => return Err(CliError::operation(&error)),
        }
    }

    save_history(&history_path, &history)?;
    Ok(())
}

fn print_repl_result(output: OutputFormat, result: &DataviewJsResult) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(result),
        OutputFormat::Human => {
            print_repl_result_human(result);
            Ok(())
        }
    }
}

fn print_repl_error(output: OutputFormat, error: &str) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(&serde_json::json!({ "error": error })),
        OutputFormat::Human => {
            eprintln!("error: {error}");
            Ok(())
        }
    }
}

fn print_repl_result_human(result: &DataviewJsResult) {
    if !result.outputs.is_empty() {
        print_outputs_human(&result.outputs);
        return;
    }

    if let Some(value) = &result.value {
        print_value_human(value);
    }
}

fn print_outputs_human(outputs: &[DataviewJsOutput]) {
    for (index, output) in outputs.iter().enumerate() {
        if index > 0 {
            println!();
        }
        match output {
            DataviewJsOutput::Table { headers, rows } => {
                if !headers.is_empty() {
                    println!("{}", headers.join(" | "));
                }
                for row in rows {
                    println!(
                        "{}",
                        row.iter()
                            .map(render_dataview_inline_value)
                            .collect::<Vec<_>>()
                            .join(" | ")
                    );
                }
            }
            DataviewJsOutput::List { items } => {
                for item in items {
                    println!("- {}", render_dataview_inline_value(item));
                }
            }
            DataviewJsOutput::TaskList {
                tasks,
                group_by_file,
            } => {
                let mut current_file: Option<&str> = None;
                for task in tasks {
                    let path = task
                        .get("path")
                        .and_then(Value::as_str)
                        .or_else(|| {
                            task.get("file")
                                .and_then(|file| file.get("path"))
                                .and_then(Value::as_str)
                        })
                        .unwrap_or("<unknown>");
                    if *group_by_file && current_file != Some(path) {
                        current_file = Some(path);
                        println!("{path}");
                    }
                    let status = task.get("status").and_then(Value::as_str).unwrap_or(" ");
                    let text = task
                        .get("text")
                        .map(render_dataview_inline_value)
                        .unwrap_or_default();
                    println!("- [{status}] {text}");
                }
            }
            DataviewJsOutput::Paragraph { text } | DataviewJsOutput::Span { text } => {
                println!("{text}");
            }
            DataviewJsOutput::Header { level, text } => {
                println!("{} {}", "#".repeat((*level).max(1)), text);
            }
            DataviewJsOutput::Element { element, text, .. } => {
                println!("<{element}> {text}");
            }
            DataviewJsOutput::Query { result } => {
                print_value_human(&serde_json::to_value(result).unwrap_or(Value::Null));
            }
        }
    }
}

fn print_value_human(value: &Value) {
    if let Some(rows) = note_collection_rows(value) {
        let path_width = rows
            .iter()
            .map(|(path, _)| path.len())
            .max()
            .unwrap_or(4)
            .max(4);
        println!("{:<path_width$} | Name", "Path");
        println!("{}-+-{}", "-".repeat(path_width), "-".repeat(24));
        for (path, name) in rows {
            println!("{path:<path_width$} | {name}");
        }
        return;
    }

    match value {
        Value::Object(_) | Value::Array(_) => print_pretty_json(value),
        _ => println!("{}", render_dataview_inline_value(value)),
    }
}

fn note_collection_rows(value: &Value) -> Option<Vec<(String, String)>> {
    let items = value.as_array()?;
    if items.is_empty() {
        return Some(Vec::new());
    }

    items
        .iter()
        .map(note_collection_row)
        .collect::<Option<Vec<_>>>()
}

fn note_collection_row(value: &Value) -> Option<(String, String)> {
    let object = value.as_object()?;
    let file = object.get("file").and_then(Value::as_object);
    let path = object.get("path").and_then(Value::as_str).or_else(|| {
        file.and_then(|file| file.get("path"))
            .and_then(Value::as_str)
    })?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            file.and_then(|file| file.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or(path);
    Some((path.to_string(), name.to_string()))
}

fn print_pretty_json(value: &Value) {
    let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    if !io::stdout().is_terminal() {
        println!("{rendered}");
        return;
    }

    for line in rendered.lines() {
        println!("{}", colorize_json_line(line));
    }
}

fn colorize_json_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len().saturating_sub(trimmed.len())];
    if trimmed.starts_with('{')
        || trimmed.starts_with('}')
        || trimmed.starts_with('[')
        || trimmed.starts_with(']')
    {
        return format!("{indent}\u{1b}[2m{trimmed}\u{1b}[0m");
    }

    if trimmed.starts_with('"') {
        if let Some((key, rest)) = trimmed.split_once(':') {
            return format!("{indent}\u{1b}[36m{key}\u{1b}[0m:{rest}");
        }
    }

    line.to_string()
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(CliError::operation)?
    );
    Ok(())
}

type ReplEditor = Editor<ReplHelper, DefaultHistory>;

#[derive(Debug, Clone)]
struct ReplHelper {
    completions: Vec<String>,
}

impl ReplHelper {
    fn new(completions: &[&str]) -> Self {
        Self {
            completions: completions.iter().map(|item| (*item).to_string()).collect(),
        }
    }

    fn completion_candidates(&self, line: &str, pos: usize) -> Option<(usize, Vec<String>)> {
        if pos > line.len() {
            return None;
        }
        let (start, prefix) = completion_prefix(&line[..pos])?;
        let matches = self
            .completions
            .iter()
            .filter(|candidate| candidate.starts_with(prefix))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        (!matches.is_empty()).then_some((start, matches))
    }
}

impl Helper for ReplHelper {}

impl Hinter for ReplHelper {
    type Hint = String;
}

impl Highlighter for ReplHelper {}

impl Validator for ReplHelper {}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let Some((start, matches)) = self.completion_candidates(line, pos) else {
            return Ok((pos, Vec::new()));
        };
        Ok((
            start,
            matches
                .into_iter()
                .map(|candidate| Pair {
                    display: candidate.clone(),
                    replacement: candidate,
                })
                .collect(),
        ))
    }
}

fn build_repl_editor(history: &[String], completions: &[&str]) -> Result<ReplEditor, CliError> {
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .build();
    let mut editor = Editor::<ReplHelper, DefaultHistory>::with_config(config)
        .map_err(|error| CliError::operation(&error))?;
    editor.set_helper(Some(ReplHelper::new(completions)));
    for entry in history {
        let _ = editor
            .add_history_entry(entry.as_str())
            .map_err(|error| CliError::operation(&error))?;
    }
    Ok(editor)
}

fn load_history(path: &Path) -> Result<Vec<String>, CliError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)
        .map_err(CliError::operation)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<String>(line).map_err(CliError::operation))
        .collect()
}

fn save_history(path: &Path, history: &[String]) -> Result<(), CliError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(CliError::operation)?;
    }
    let contents = history
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(CliError::operation)?
        .join("\n");
    if contents.is_empty() {
        fs::write(path, "").map_err(CliError::operation)
    } else {
        fs::write(path, format!("{contents}\n")).map_err(CliError::operation)
    }
}

fn record_submission(history: &mut Vec<String>, submission: &str) -> bool {
    if submission.trim().is_empty() || history.last().is_some_and(|last| last == submission) {
        return false;
    }
    history.push(submission.to_string());
    if history.len() > MAX_HISTORY_ENTRIES {
        history.remove(0);
    }
    true
}

fn completion_prefix(buffer: &str) -> Option<(usize, &str)> {
    if buffer.is_empty() {
        return None;
    }
    let start = buffer
        .char_indices()
        .rev()
        .find(|(_, ch)| !(ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-')))
        .map_or(0, |(index, ch)| index + ch.len_utf8());
    let prefix = &buffer[start..];
    (!prefix.is_empty()).then_some((start, prefix))
}

fn repl_input_needs_continuation(source: &str) -> bool {
    let trimmed = source.trim_end();
    if trimmed.is_empty() {
        return false;
    }

    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut braces = 0usize;
    let mut chars = source.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut escape = false;
    let mut line_comment = false;
    let mut block_comment = false;

    while let Some(ch) = chars.next() {
        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            continue;
        }
        if block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_comment = false;
            }
            continue;
        }
        if in_single {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_double = false;
            }
            continue;
        }
        if in_template {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '`' {
                in_template = false;
            }
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            line_comment = true;
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            block_comment = true;
            continue;
        }

        match ch {
            '\'' => in_single = true,
            '"' => in_double = true,
            '`' => in_template = true,
            '(' => parens += 1,
            ')' => parens = parens.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            _ => {}
        }
    }

    in_single
        || in_double
        || in_template
        || line_comment
        || block_comment
        || parens > 0
        || brackets > 0
        || braces > 0
        || matches!(
            trimmed.chars().last(),
            Some('.' | ',' | ':' | '=' | '\\' | '+' | '-' | '*' | '/')
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn continuation_detection_covers_braces_templates_and_trailing_operators() {
        assert!(repl_input_needs_continuation("if (true) {"));
        assert!(repl_input_needs_continuation("const value = `alpha"));
        assert!(repl_input_needs_continuation("vault."));
        assert!(!repl_input_needs_continuation("({ ok: true })"));
        assert!(!repl_input_needs_continuation("vault.note(\"Home\")"));
    }

    #[test]
    fn completion_expands_namespaces_and_lists_ambiguous_matches() {
        let helper = ReplHelper::new(REPL_COMPLETIONS);

        let (graph_start, graph_matches) = helper
            .completion_candidates("vault.gr", "vault.gr".len())
            .expect("completion candidates should exist");
        assert_eq!(graph_start, 0);
        assert!(graph_matches.iter().any(|item| item == "vault.graph."));

        let (_, matches) = helper
            .completion_candidates("vault.", "vault.".len())
            .expect("completion candidates should exist");
        assert!(matches.iter().any(|item| item == "vault.note("));
        assert!(matches.iter().any(|item| item == "vault.graph."));
    }

    #[test]
    fn history_round_trips_multiline_entries() -> Result<(), CliError> {
        let temp_dir = tempdir().expect("temp dir should be created");
        let history_path = temp_dir.path().join("history.jsonl");
        let history = vec![
            "1 + 1".to_string(),
            "const note = vault.note(\"Home\");\nnote.path".to_string(),
        ];
        save_history(&history_path, &history)?;

        let loaded = load_history(&history_path)?;
        assert_eq!(loaded, history);
        Ok(())
    }

    #[test]
    fn record_submission_deduplicates_and_caps_history() {
        let mut history = vec!["0".to_string()];
        assert!(record_submission(&mut history, "1 + 1"));
        assert!(!record_submission(&mut history, "1 + 1"));

        history.clear();
        for index in 0..=MAX_HISTORY_ENTRIES {
            let _ = record_submission(&mut history, &index.to_string());
        }
        let expected_last = MAX_HISTORY_ENTRIES.to_string();
        assert_eq!(history.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(history.first().map(String::as_str), Some("1"));
        assert_eq!(
            history.last().map(String::as_str),
            Some(expected_last.as_str())
        );
    }
}
