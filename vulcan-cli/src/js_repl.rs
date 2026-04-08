use crate::output::{markdown_table_column_count, markdown_table_header_lines, markdown_table_row};
use crate::{render_dataview_inline_value, trust, CliError, OutputFormat};
use rustyline::completion::{Completer, Pair};
use rustyline::config::{CompletionType, Config, EditMode};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use serde::Serialize;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::time::{Duration, Instant};
use vulcan_core::{
    DataviewJsEvalOptions, DataviewJsOutput, DataviewJsResult, DataviewJsSession, JsRuntimeSandbox,
    VaultPaths,
};

const PRIMARY_PROMPT: &str = "vulcan> ";
const CONTINUATION_PROMPT: &str = "......> ";
const MAX_HISTORY_ENTRIES: usize = 10_000;
const REPL_COMPLETIONS: &[&str] = &[
    // Dot commands
    ".exit",
    ".quit",
    ".type ",
    ".keys ",
    ".inspect ",
    ".time ",
    ".bench ",
    ".source ",
    // console
    "console.log(",
    // help
    "help(",
    "help(vault)",
    "help(dv)",
    "help(web)",
    "help(console)",
    "help(app)",
    // vault
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
    // dv (DataView)
    "dv.",
    "dv.current(",
    "dv.page(",
    "dv.pages(",
    "dv.table(",
    "dv.list(",
    "dv.taskList(",
    "dv.paragraph(",
    "dv.header(",
    "dv.span(",
    "dv.el(",
    "dv.execute(",
    "dv.io.",
    "dv.io.load(",
    "dv.io.csv(",
    "dv.io.normalize(",
    "dv.func.",
    // web
    "web.search(",
    "web.fetch(",
    // app (Obsidian compat)
    "app.",
    "app.vault.",
    "app.vault.getName(",
    "app.vault.getMarkdownFiles(",
    "app.vault.read(",
    "app.vault.modify(",
    "app.vault.getAbstractFileByPath(",
];

pub(crate) fn run_js_repl(
    paths: &VaultPaths,
    output: OutputFormat,
    timeout: Option<Duration>,
    sandbox: Option<JsRuntimeSandbox>,
    permission_profile: Option<&str>,
    no_startup: bool,
) -> Result<(), CliError> {
    let session = DataviewJsSession::new(
        paths,
        None,
        DataviewJsEvalOptions {
            timeout,
            sandbox,
            permission_profile: permission_profile.map(ToOwned::to_owned),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(CliError::operation)?;
    fs::create_dir_all(paths.vulcan_dir()).map_err(CliError::operation)?;

    // Auto-load startup script when the vault is trusted and the file exists.
    if !no_startup {
        let startup_path = paths.vulcan_dir().join("scripts").join("startup.js");
        if startup_path.exists() {
            if trust::is_trusted(paths.vault_root()) {
                eprintln!("Loading {}...", startup_path.display());
                let source = fs::read_to_string(&startup_path).map_err(CliError::operation)?;
                match session.evaluate(&source).map_err(CliError::operation) {
                    Ok(result) => {
                        inject_last_result(&session, &result);
                        print_repl_result(output, &result)?;
                    }
                    Err(error) => {
                        let msg = friendly_repl_error(&error.to_string());
                        inject_last_error(&session, &msg);
                        print_repl_error(output, &msg)?;
                    }
                }
            } else {
                eprintln!(
                    "Note: .vulcan/scripts/startup.js exists but vault is not trusted — \
                     run `vulcan trust` to enable startup scripts."
                );
            }
        }
    }

    run_repl_loop(&session, paths, output)
}

/// Start the REPL after pre-loading a JS file.  The file is evaluated in the session and its
/// result is printed before entering the interactive loop.
pub(crate) fn run_js_repl_with_preload(
    paths: &VaultPaths,
    output: OutputFormat,
    timeout: Option<Duration>,
    sandbox: Option<JsRuntimeSandbox>,
    permission_profile: Option<&str>,
    preload_path: &str,
) -> Result<(), CliError> {
    let session = DataviewJsSession::new(
        paths,
        None,
        DataviewJsEvalOptions {
            timeout,
            sandbox,
            permission_profile: permission_profile.map(ToOwned::to_owned),
            ..DataviewJsEvalOptions::default()
        },
    )
    .map_err(CliError::operation)?;

    // Load and evaluate the preload file before entering the REPL.
    let source = std::fs::read_to_string(preload_path).map_err(|error| {
        CliError::operation(format!("failed to read eval-file {preload_path}: {error}"))
    })?;
    eprintln!("Loading {preload_path}...");
    match session.evaluate(&source).map_err(CliError::operation) {
        Ok(result) => {
            inject_last_result(&session, &result);
            print_repl_result(output, &result)?;
        }
        Err(error) => {
            let msg = friendly_repl_error(&error.to_string());
            inject_last_error(&session, &msg);
            print_repl_error(output, &msg)?;
        }
    }

    run_repl_loop(&session, paths, output)
}

fn run_repl_loop(
    session: &DataviewJsSession,
    paths: &VaultPaths,
    output: OutputFormat,
) -> Result<(), CliError> {
    let history_path = paths.vulcan_dir().join("repl_history");
    let mut history = load_history(&history_path)?;
    let mut editor = build_repl_editor(&history, REPL_COMPLETIONS)?;
    let mut pending = String::new();

    loop {
        match read_repl_submission(&mut editor, &pending) {
            Ok(line) => {
                if repl_input_needs_continuation(&line) {
                    pending = line;
                    continue;
                }

                pending.clear();
                let source = line;
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

                if !try_dot_command(session, trimmed, output)? {
                    match session.evaluate(&source).map_err(CliError::operation) {
                        Ok(result) => {
                            print_repl_result(output, &result)?;
                            inject_last_result(session, &result);
                        }
                        Err(error) => {
                            let msg = friendly_repl_error(&error.to_string());
                            print_repl_error(output, &msg)?;
                            inject_last_error(session, &msg);
                        }
                    }
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

fn read_repl_submission(editor: &mut ReplEditor, pending: &str) -> Result<String, ReadlineError> {
    if pending.is_empty() {
        editor.readline(PRIMARY_PROMPT)
    } else {
        let initial = continuation_initial_buffer(pending);
        editor.readline_with_initial(CONTINUATION_PROMPT, (&initial, ""))
    }
}

fn continuation_initial_buffer(pending: &str) -> String {
    let mut initial = pending.to_string();
    if !initial.ends_with('\n') {
        initial.push('\n');
    }
    initial
}

fn inject_last_result(session: &DataviewJsSession, result: &DataviewJsResult) {
    if let Some(value) = &result.value {
        if let Ok(json) = serde_json::to_string(value) {
            // Escape the JSON string itself so it can be embedded as a JS string literal.
            if let Ok(escaped) = serde_json::to_string(&json) {
                let _ = session.evaluate(&format!("globalThis._ = JSON.parse({escaped});"));
            }
        }
    } else {
        let _ = session.evaluate("globalThis._ = undefined;");
    }
}

fn inject_last_error(session: &DataviewJsSession, msg: &str) {
    if let Ok(escaped) = serde_json::to_string(msg) {
        let _ = session.evaluate(&format!("globalThis._error = {escaped};"));
    }
}

fn friendly_repl_error(raw: &str) -> String {
    if raw.contains("Error converting from js 'undefined' into type 'string'")
        || raw.contains("Error converting from js")
    {
        return format!(
            "{raw}\n\
             Tip: if you typed a function name like `help` or `vault` without parentheses, \
             add them: `help()`, `vault.note(\"path\")`"
        );
    }
    raw.to_string()
}

#[allow(clippy::too_many_lines)]
fn try_dot_command(
    session: &DataviewJsSession,
    trimmed: &str,
    output: OutputFormat,
) -> Result<bool, CliError> {
    let Some(rest) = trimmed.strip_prefix('.') else {
        return Ok(false);
    };
    // Distinguish from known exit commands (already handled by caller)
    let (cmd, arg) = rest
        .split_once(' ')
        .map_or((rest, ""), |(c, a)| (c, a.trim()));

    match cmd {
        "type" => {
            if arg.is_empty() {
                eprintln!("usage: .type <expr>");
                return Ok(true);
            }
            let js = format!("typeof ({arg})");
            run_dot_expr(session, &js, output)?;
        }
        "keys" => {
            if arg.is_empty() {
                eprintln!("usage: .keys <expr>");
                return Ok(true);
            }
            let js = format!("Object.keys({arg})");
            run_dot_expr(session, &js, output)?;
        }
        "inspect" => {
            if arg.is_empty() {
                eprintln!("usage: .inspect <expr>");
                return Ok(true);
            }
            let js = format!(
                r#"(function(v) {{
                  if (v === null) return "null";
                  if (typeof v !== "object" && typeof v !== "function") return typeof v + ": " + JSON.stringify(v);
                  const keys = Object.keys(v);
                  return keys.map(k => k + ": " + typeof v[k]).join("\n");
                }})({arg})"#
            );
            run_dot_expr(session, &js, output)?;
        }
        "time" => {
            if arg.is_empty() {
                eprintln!("usage: .time <expr>");
                return Ok(true);
            }
            let start = Instant::now();
            match session.evaluate(arg).map_err(CliError::operation) {
                Ok(result) => {
                    eprintln!("Elapsed: {:.3}ms", start.elapsed().as_secs_f64() * 1000.0);
                    print_repl_result(output, &result)?;
                }
                Err(error) => print_repl_error(output, &friendly_repl_error(&error.to_string()))?,
            }
        }
        "bench" => {
            let (expr, n) = if let Some(space_pos) = arg.rfind(' ') {
                let potential_n = &arg[space_pos + 1..];
                if let Ok(count) = potential_n.parse::<u32>() {
                    (&arg[..space_pos], count)
                } else {
                    (arg, 100u32)
                }
            } else {
                (arg, 100u32)
            };
            if expr.is_empty() {
                eprintln!("usage: .bench <expr> [n]");
                return Ok(true);
            }
            let start = Instant::now();
            let mut last_result: Option<DataviewJsResult> = None;
            let mut errored = false;
            for _ in 0..n {
                match session.evaluate(expr).map_err(CliError::operation) {
                    Ok(r) => {
                        last_result = Some(r);
                    }
                    Err(error) => {
                        print_repl_error(output, &friendly_repl_error(&error.to_string()))?;
                        errored = true;
                        break;
                    }
                }
            }
            if !errored {
                let total_ms = start.elapsed().as_secs_f64() * 1000.0;
                let avg_ms = total_ms / f64::from(n);
                eprintln!("{n} iterations: total {total_ms:.3}ms, avg {avg_ms:.3}ms/iter");
                if let Some(r) = last_result {
                    print_repl_result(output, &r)?;
                }
            }
        }
        "source" => {
            if arg.is_empty() {
                eprintln!("usage: .source <fn>");
                return Ok(true);
            }
            let js = format!("({arg}).toString()");
            run_dot_expr(session, &js, output)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn run_dot_expr(
    session: &DataviewJsSession,
    js: &str,
    output: OutputFormat,
) -> Result<(), CliError> {
    match session.evaluate(js).map_err(CliError::operation) {
        Ok(result) => print_repl_result(output, &result),
        Err(error) => print_repl_error(output, &friendly_repl_error(&error.to_string())),
    }
}

fn print_repl_result(output: OutputFormat, result: &DataviewJsResult) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(result),
        OutputFormat::Human | OutputFormat::Markdown => {
            print_repl_result_human(result);
            Ok(())
        }
    }
}

fn print_repl_error(output: OutputFormat, error: &str) -> Result<(), CliError> {
    match output {
        OutputFormat::Json => print_json(&serde_json::json!({ "error": error })),
        OutputFormat::Human | OutputFormat::Markdown => {
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
                let column_count =
                    markdown_table_column_count(headers.len(), rows.iter().map(Vec::len));
                if column_count > 0 {
                    let [header, separator] = markdown_table_header_lines(headers, column_count);
                    println!("{header}");
                    println!("{separator}");
                }
                for row in rows {
                    println!(
                        "{}",
                        markdown_table_row(
                            row.iter().map(render_dataview_inline_value),
                            column_count,
                        )
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

#[allow(clippy::too_many_lines)]
fn highlight_js_line(line: &str) -> String {
    const KEYWORD_COLOR: &str = "\x1b[35m"; // magenta
    const STRING_COLOR: &str = "\x1b[32m"; // green
    const NUMBER_COLOR: &str = "\x1b[33m"; // yellow
    const COMMENT_COLOR: &str = "\x1b[2m"; // dim
    const RESET: &str = "\x1b[0m";
    const KEYWORDS: &[&str] = &[
        "const",
        "let",
        "var",
        "function",
        "return",
        "if",
        "else",
        "for",
        "while",
        "new",
        "typeof",
        "instanceof",
        "null",
        "undefined",
        "true",
        "false",
        "class",
        "this",
        "import",
        "export",
        "await",
        "async",
        "throw",
        "try",
        "catch",
        "finally",
        "break",
        "continue",
        "delete",
        "in",
        "of",
        "switch",
        "case",
        "default",
        "void",
    ];

    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(line.len() * 2);
    let mut i = 0;

    while i < len {
        // Line comment
        if i + 1 < len && chars[i] == '/' && chars[i + 1] == '/' {
            out.push_str(COMMENT_COLOR);
            while i < len {
                out.push(chars[i]);
                i += 1;
            }
            out.push_str(RESET);
            break;
        }

        // String literals
        if matches!(chars[i], '\'' | '"' | '`') {
            let delim = chars[i];
            out.push_str(STRING_COLOR);
            out.push(chars[i]);
            i += 1;
            while i < len {
                let ch = chars[i];
                out.push(ch);
                i += 1;
                if ch == '\\' && i < len {
                    out.push(chars[i]);
                    i += 1;
                    continue;
                }
                if ch == delim {
                    break;
                }
            }
            out.push_str(RESET);
            continue;
        }

        // Numbers
        if chars[i].is_ascii_digit() {
            out.push_str(NUMBER_COLOR);
            while i < len && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                out.push(chars[i]);
                i += 1;
            }
            out.push_str(RESET);
            continue;
        }

        // Keywords / identifiers
        if chars[i].is_alphabetic() || chars[i] == '_' || chars[i] == '$' {
            let start = i;
            while i < len && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if KEYWORDS.contains(&word.as_str()) {
                out.push_str(KEYWORD_COLOR);
                out.push_str(&word);
                out.push_str(RESET);
            } else {
                out.push_str(&word);
            }
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    out
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

impl Highlighter for ReplHelper {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        if io::stdout().is_terminal() {
            Cow::Owned(highlight_js_line(line))
        } else {
            Cow::Borrowed(line)
        }
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _forced: bool) -> bool {
        io::stdout().is_terminal()
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        if default && io::stdout().is_terminal() {
            Cow::Owned(format!("\x1b[2m{prompt}\x1b[0m"))
        } else {
            Cow::Borrowed(prompt)
        }
    }
}

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
        .edit_mode(EditMode::Emacs)
        .check_cursor_position(true)
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
    fn continuation_initial_buffer_appends_one_newline_for_multiline_editing() {
        assert_eq!(continuation_initial_buffer("if (true) {"), "if (true) {\n");
        assert_eq!(
            continuation_initial_buffer("if (true) {\n  1 + 1\n"),
            "if (true) {\n  1 + 1\n"
        );
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

    #[test]
    fn max_history_entries_is_ten_thousand() {
        assert_eq!(MAX_HISTORY_ENTRIES, 10_000);
    }

    #[test]
    fn completion_includes_dv_and_app_prefixes() {
        let helper = ReplHelper::new(REPL_COMPLETIONS);

        let (_, dv_matches) = helper
            .completion_candidates("dv.", "dv.".len())
            .expect("dv. should have completions");
        assert!(
            dv_matches.iter().any(|m| m.starts_with("dv.pages")),
            "dv. completions should include dv.pages(, got: {dv_matches:?}"
        );

        let (_, app_matches) = helper
            .completion_candidates("app.", "app.".len())
            .expect("app. should have completions");
        assert!(
            app_matches.iter().any(|m| m.starts_with("app.vault")),
            "app. completions should include app.vault., got: {app_matches:?}"
        );
    }

    #[test]
    fn completion_includes_dot_commands() {
        let helper = ReplHelper::new(REPL_COMPLETIONS);
        let (_, matches) = helper
            .completion_candidates(".t", ".t".len())
            .expect(".t should have completions");
        assert!(
            matches.iter().any(|m| m.starts_with(".type")),
            ".type should be a completion candidate, got: {matches:?}"
        );
    }

    #[test]
    fn highlight_js_line_colors_keywords_and_strings() {
        let output = highlight_js_line("const x = \"hello\";");
        // Keywords colored in magenta
        assert!(
            output.contains("\x1b[35mconst\x1b[0m"),
            "const should be highlighted in magenta, got: {output:?}"
        );
        // String in green
        assert!(
            output.contains("\x1b[32m\"hello\"\x1b[0m"),
            "string literal should be highlighted in green, got: {output:?}"
        );
    }

    #[test]
    fn highlight_js_line_colors_line_comments() {
        let output = highlight_js_line("x + 1 // side effect");
        assert!(
            output.contains("\x1b[2m// side effect\x1b[0m"),
            "line comments should be highlighted as dim, got: {output:?}"
        );
    }

    #[test]
    fn highlight_js_line_colors_numbers() {
        let output = highlight_js_line("return 42;");
        assert!(
            output.contains("\x1b[33m42\x1b[0m"),
            "numbers should be highlighted in yellow, got: {output:?}"
        );
    }

    #[test]
    fn friendly_repl_error_rewrites_undefined_conversion() {
        let raw = "error: Error converting from js 'undefined' into type 'string'";
        let friendly = friendly_repl_error(raw);
        assert!(
            friendly.contains("Tip:"),
            "should include a tip, got: {friendly}"
        );
        assert!(
            friendly.contains("parentheses"),
            "should mention parentheses, got: {friendly}"
        );
    }

    #[test]
    fn friendly_repl_error_passes_through_normal_errors() {
        let raw = "ReferenceError: x is not defined";
        let friendly = friendly_repl_error(raw);
        assert_eq!(
            friendly, raw,
            "unrelated errors should pass through unchanged"
        );
    }
}
