use crate::{render_dataview_inline_value, CliError, OutputFormat};
use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::queue;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
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
    let mut state = ReplInputState::load(history_path, REPL_COMPLETIONS)?;
    let mut stdout = io::stdout();
    let _raw_mode = RawModeGuard::enable()?;
    state.render(&mut stdout)?;

    loop {
        let Event::Key(key) = event::read().map_err(CliError::operation)? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match state.handle_key(key) {
            ReplAction::Continue => state.render(&mut stdout)?,
            ReplAction::ShowCompletions(items) => {
                state.begin_message_block(&mut stdout)?;
                if !items.is_empty() {
                    println!("{}", items.join("  "));
                }
                state.render(&mut stdout)?;
            }
            ReplAction::Submit(source) => {
                state.begin_message_block(&mut stdout)?;
                let trimmed = source.trim();
                if trimmed.is_empty() {
                    state.render(&mut stdout)?;
                    continue;
                }
                if matches!(trimmed, ".exit" | ".quit") {
                    break;
                }

                match session.evaluate(&source).map_err(CliError::operation) {
                    Ok(result) => print_repl_result(output, &result)?,
                    Err(error) => print_repl_error(output, &error.to_string())?,
                }
                state.render(&mut stdout)?;
            }
            ReplAction::Exit => {
                state.begin_message_block(&mut stdout)?;
                break;
            }
        }
    }

    state.save_history()?;
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

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self, CliError> {
        enable_raw_mode().map_err(CliError::operation)?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReplAction {
    Continue,
    ShowCompletions(Vec<String>),
    Submit(String),
    Exit,
}

#[derive(Debug, Clone)]
struct ReplInputState {
    buffer: String,
    history: Vec<String>,
    history_path: PathBuf,
    history_cursor: Option<usize>,
    saved_buffer: String,
    completions: Vec<String>,
    rendered_lines: usize,
}

impl ReplInputState {
    fn load(history_path: PathBuf, completions: &[&str]) -> Result<Self, CliError> {
        Ok(Self {
            buffer: String::new(),
            history: load_history(&history_path)?,
            history_path,
            history_cursor: None,
            saved_buffer: String::new(),
            completions: completions.iter().map(|item| (*item).to_string()).collect(),
            rendered_lines: 0,
        })
    }

    fn save_history(&self) -> Result<(), CliError> {
        if let Some(parent) = self
            .history_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(CliError::operation)?;
        }
        let contents = self
            .history
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .map_err(CliError::operation)?
            .join("\n");
        if contents.is_empty() {
            fs::write(&self.history_path, "").map_err(CliError::operation)?;
        } else {
            fs::write(&self.history_path, format!("{contents}\n")).map_err(CliError::operation)?;
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> ReplAction {
        if key.modifiers == KeyModifiers::CONTROL {
            match key.code {
                KeyCode::Char('d' | 'D') if self.buffer.is_empty() => return ReplAction::Exit,
                KeyCode::Char('c' | 'C') => {
                    self.clear_buffer();
                    return ReplAction::Continue;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => {
                if repl_input_needs_continuation(&self.buffer) {
                    self.buffer.push('\n');
                    return ReplAction::Continue;
                }
                ReplAction::Submit(self.take_submission())
            }
            KeyCode::Backspace => {
                self.buffer.pop();
                self.history_cursor = None;
                ReplAction::Continue
            }
            KeyCode::Up => {
                self.navigate_history(-1);
                ReplAction::Continue
            }
            KeyCode::Down => {
                self.navigate_history(1);
                ReplAction::Continue
            }
            KeyCode::Tab => match complete_buffer(&mut self.buffer, &self.completions) {
                CompletionOutcome::None | CompletionOutcome::Applied => ReplAction::Continue,
                CompletionOutcome::Choices(items) => ReplAction::ShowCompletions(items),
            },
            KeyCode::Char(character)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.buffer.push(character);
                self.history_cursor = None;
                ReplAction::Continue
            }
            _ => ReplAction::Continue,
        }
    }

    fn render(&mut self, stdout: &mut io::Stdout) -> Result<(), CliError> {
        if self.rendered_lines > 0 {
            queue!(
                stdout,
                MoveUp(u16::try_from(self.rendered_lines.saturating_sub(1)).unwrap_or(u16::MAX)),
                MoveToColumn(0),
                Clear(ClearType::FromCursorDown)
            )
            .map_err(CliError::operation)?;
        }

        let lines = render_prompt_lines(&self.buffer);
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                writeln!(stdout).map_err(CliError::operation)?;
            }
            write!(stdout, "{line}").map_err(CliError::operation)?;
        }
        stdout.flush().map_err(CliError::operation)?;
        self.rendered_lines = lines.len();
        Ok(())
    }

    fn begin_message_block(&mut self, stdout: &mut io::Stdout) -> Result<(), CliError> {
        if self.rendered_lines > 0 {
            writeln!(stdout).map_err(CliError::operation)?;
            stdout.flush().map_err(CliError::operation)?;
        }
        self.rendered_lines = 0;
        Ok(())
    }

    fn clear_buffer(&mut self) {
        self.buffer.clear();
        self.history_cursor = None;
        self.saved_buffer.clear();
    }

    fn take_submission(&mut self) -> String {
        let submission = std::mem::take(&mut self.buffer);
        self.history_cursor = None;
        self.saved_buffer.clear();
        let trimmed = submission.trim();
        if !trimmed.is_empty() && self.history.last() != Some(&submission) {
            self.history.push(submission.clone());
            if self.history.len() > MAX_HISTORY_ENTRIES {
                self.history.remove(0);
            }
        }
        submission
    }

    fn navigate_history(&mut self, delta: isize) {
        if self.history.is_empty() {
            return;
        }

        match (self.history_cursor, delta) {
            (None, -1) => {
                self.saved_buffer = self.buffer.clone();
                self.history_cursor = Some(self.history.len() - 1);
            }
            (Some(index), -1) if index > 0 => {
                self.history_cursor = Some(index - 1);
            }
            (Some(index), 1) if index + 1 < self.history.len() => {
                self.history_cursor = Some(index + 1);
            }
            (Some(_), 1) => {
                self.history_cursor = None;
                self.buffer.clone_from(&self.saved_buffer);
                return;
            }
            _ => return,
        }

        if let Some(index) = self.history_cursor {
            self.buffer.clone_from(&self.history[index]);
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompletionOutcome {
    None,
    Applied,
    Choices(Vec<String>),
}

fn complete_buffer(buffer: &mut String, completions: &[String]) -> CompletionOutcome {
    let Some((start, prefix)) = completion_prefix(buffer) else {
        return CompletionOutcome::None;
    };

    let matches = completions
        .iter()
        .filter(|candidate| candidate.starts_with(prefix))
        .cloned()
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return CompletionOutcome::None;
    }

    let replacement = longest_common_prefix(&matches);
    if replacement.len() > prefix.len() {
        buffer.replace_range(start.., &replacement);
        return CompletionOutcome::Applied;
    }

    if matches.len() == 1 {
        buffer.replace_range(start.., &matches[0]);
        return CompletionOutcome::Applied;
    }

    let choices = matches
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    CompletionOutcome::Choices(choices)
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

fn longest_common_prefix(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return String::new();
    };
    let mut prefix = first.clone();
    for candidate in &values[1..] {
        while !candidate.starts_with(&prefix) {
            prefix.pop();
            if prefix.is_empty() {
                break;
            }
        }
    }
    prefix
}

fn render_prompt_lines(buffer: &str) -> Vec<String> {
    if buffer.is_empty() {
        return vec![PRIMARY_PROMPT.to_string()];
    }

    let mut lines = buffer
        .split('\n')
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("{PRIMARY_PROMPT}{line}")
            } else {
                format!("{CONTINUATION_PROMPT}{line}")
            }
        })
        .collect::<Vec<_>>();
    if buffer.ends_with('\n') {
        lines.push(CONTINUATION_PROMPT.to_string());
    }
    lines
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn continuation_detection_covers_braces_templates_and_trailing_operators() {
        assert!(repl_input_needs_continuation("if (true) {"));
        assert!(repl_input_needs_continuation("const value = `alpha"));
        assert!(repl_input_needs_continuation("vault."));
        assert!(!repl_input_needs_continuation("({ ok: true })"));
        assert!(!repl_input_needs_continuation("vault.note(\"Home\")"));
    }

    #[test]
    fn completion_expands_namespaces_and_lists_ambiguous_matches() -> Result<(), CliError> {
        let temp_dir = tempdir().expect("temp dir should be created");
        let mut state =
            ReplInputState::load(temp_dir.path().join("history.jsonl"), REPL_COMPLETIONS)?;

        state.buffer = "vault.gr".to_string();
        assert_eq!(state.handle_key(key(KeyCode::Tab)), ReplAction::Continue);
        assert_eq!(state.buffer, "vault.graph.");

        state.buffer = "vault.".to_string();
        let action = state.handle_key(key(KeyCode::Tab));
        assert!(
            matches!(action, ReplAction::ShowCompletions(items) if items.iter().any(|item| item == "vault.note(") && items.iter().any(|item| item == "vault.graph."))
        );
        Ok(())
    }

    #[test]
    fn history_round_trips_multiline_entries() -> Result<(), CliError> {
        let temp_dir = tempdir().expect("temp dir should be created");
        let history_path = temp_dir.path().join("history.jsonl");
        let mut state = ReplInputState::load(history_path.clone(), REPL_COMPLETIONS)?;
        state.history = vec![
            "1 + 1".to_string(),
            "const note = vault.note(\"Home\");\nnote.path".to_string(),
        ];
        state.save_history()?;

        let loaded = load_history(&history_path)?;
        assert_eq!(loaded, state.history);
        Ok(())
    }

    #[test]
    fn input_state_submits_complete_forms_and_navigates_history() -> Result<(), CliError> {
        let temp_dir = tempdir().expect("temp dir should be created");
        let mut state =
            ReplInputState::load(temp_dir.path().join("history.jsonl"), REPL_COMPLETIONS)?;
        state.buffer = "1 + 1".to_string();
        assert_eq!(
            state.handle_key(key(KeyCode::Enter)),
            ReplAction::Submit("1 + 1".to_string())
        );
        assert_eq!(state.history, vec!["1 + 1".to_string()]);

        state.buffer = "pending".to_string();
        assert_eq!(state.handle_key(key(KeyCode::Up)), ReplAction::Continue);
        assert_eq!(state.buffer, "1 + 1");
        assert_eq!(state.handle_key(key(KeyCode::Down)), ReplAction::Continue);
        assert_eq!(state.buffer, "pending");
        Ok(())
    }
}
