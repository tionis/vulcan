//! Numerals-compatible calculator blocks and inline calculations.
//!
//! The Obsidian Numerals plugin evaluates ` ```math ` blocks line by line
//! with mathjs, and inline code such as `` `#: 2 + 2` `` in prose. Vulcan
//! runs the same mathjs release (vendored in `vendor/mathjs/`) and a port of
//! the plugin's processing (`numerals_prelude.js`) in an isolated `QuickJS`
//! engine with no vault access: a note's frontmatter, inline fields, and the
//! notes its `[[Note]].property` references name are handed in as JSON.
//!
//! Rendering only reads. A block's `@[name::result]` insertions are written
//! by [`numerals_insertion_edits`] through `vulcan numerals apply`. See
//! `docs/specs/live-documents.md` (9.42.4).

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{NumeralsConfig, NumeralsRenderStyle};
use crate::parser::types::{InlineNumeralsMode, ParsedDocument};

/// One shown line of an evaluated block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumeralsLine {
    /// The line's index inside the block.
    pub line: usize,
    pub input: String,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub emitter: bool,
    /// The formatted result; `None` for lines without one (blank lines,
    /// comments, assignments of functions that produce nothing).
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tex_input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tex_result: Option<String>,
}

/// The line that stopped a block's evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumeralsLineError {
    pub name: String,
    pub message: String,
    pub input: String,
}

/// A `@[name::result]` value the block computed for its line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumeralsInsertion {
    pub line: usize,
    pub value: String,
}

/// An evaluated block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumeralsBlockOutput {
    pub lines: Vec<NumeralsLine>,
    #[serde(default)]
    pub error: Option<NumeralsLineError>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub insertions: Vec<NumeralsInsertion>,
    #[serde(default)]
    pub emitters: bool,
}

/// What one Numerals region of a note evaluated to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NumeralsOutcome {
    Block {
        style: NumeralsStyle,
        separator: String,
        output: NumeralsBlockOutput,
    },
    Inline {
        text: String,
        tex: bool,
    },
    Error {
        message: String,
        /// Whether the region is an inline calculation rather than a block.
        inline: bool,
    },
}

/// A block's render style, as its fence language or the config picks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NumeralsStyle {
    Plain,
    Tex,
    Highlight,
}

/// One evaluated region, in document order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NumeralsRegion {
    pub byte_range: Range<usize>,
    pub line_number: usize,
    pub outcome: NumeralsOutcome,
}

/// What evaluation knows about the note: its metadata (frontmatter and
/// inline fields) and the referenced notes' metadata, by link text.
pub struct NumeralsNote<'a> {
    pub source: &'a str,
    pub parsed: &'a ParsedDocument,
    pub metadata: Value,
    pub references: BTreeMap<String, Value>,
}

/// The style a block's fence language selects; `math` uses the configured
/// default.
#[must_use]
pub fn block_style(language: &str, config: &NumeralsConfig) -> NumeralsStyle {
    match language {
        "math-plain" => NumeralsStyle::Plain,
        "math-tex" | "math-TeX" => NumeralsStyle::Tex,
        "math-highlight" => NumeralsStyle::Highlight,
        _ => match config.render_style {
            NumeralsRenderStyle::Plain => NumeralsStyle::Plain,
            NumeralsRenderStyle::Tex => NumeralsStyle::Tex,
            NumeralsRenderStyle::Highlight => NumeralsStyle::Highlight,
        },
    }
}

fn cross_note_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\[\[([^\]]+)\]\]\.[$\w\x{00C0}-\x{02AF}\x{0370}-\x{03FF}\x{2100}-\x{214F}]")
            .expect("cross-note reference regex should compile")
    })
}

/// The note names the note's Numerals regions reference as
/// `[[Note]].property`, for the caller to resolve.
#[must_use]
pub fn cross_note_reference_names(parsed: &ParsedDocument) -> Vec<String> {
    let mut names = parsed
        .numerals_blocks
        .iter()
        .map(|block| block.text.as_str())
        .chain(
            parsed
                .inline_numerals
                .iter()
                .map(|inline| inline.expression.as_str()),
        )
        .flat_map(|text| {
            cross_note_regex()
                .captures_iter(text)
                .map(|captures| captures[1].to_string())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

/// Whether the note has Numerals regions to evaluate under `config`.
#[must_use]
pub fn has_numerals(parsed: &ParsedDocument, config: &NumeralsConfig) -> bool {
    config.enabled && (!parsed.numerals_blocks.is_empty() || !parsed.inline_numerals.is_empty())
}

enum Region<'p> {
    Block(&'p crate::parser::types::RawNumeralsBlock),
    Inline(&'p crate::parser::types::RawInlineNumerals),
}

impl Region<'_> {
    fn range(&self) -> &Range<usize> {
        match self {
            Region::Block(block) => &block.byte_range,
            Region::Inline(inline) => &inline.byte_range,
        }
    }

    fn line_number(&self) -> usize {
        match self {
            Region::Block(block) => block.line_number,
            Region::Inline(inline) => inline.line_number,
        }
    }
}

/// Evaluate the note's Numerals blocks and inline calculations in document
/// order. Blocks share `$`-prefixed globals with every later region, as the
/// plugin's page scope does, and consecutive inline calculations in one
/// paragraph share `@prev`.
#[must_use]
pub fn evaluate_note_numerals(
    config: &crate::config::VaultConfig,
    note: &NumeralsNote<'_>,
) -> Vec<NumeralsRegion> {
    let numerals = &config.numerals;
    if !numerals.enabled {
        return Vec::new();
    }
    let mut regions = note
        .parsed
        .numerals_blocks
        .iter()
        .map(Region::Block)
        .chain(note.parsed.inline_numerals.iter().map(Region::Inline))
        .collect::<Vec<_>>();
    regions.sort_by_key(|region| region.range().start);
    if regions.is_empty() {
        return Vec::new();
    }

    let mut engine = engine::NoteEngine::start(config, note);
    let mut previous_inline_end = None;
    regions
        .into_iter()
        .map(|region| {
            let outcome = match &region {
                Region::Block(block) => {
                    previous_inline_end = None;
                    let style = block_style(&block.language, numerals);
                    match engine.block(&block.text, style) {
                        Ok(output) => NumeralsOutcome::Block {
                            style,
                            separator: numerals.result_separator.clone(),
                            output,
                        },
                        Err(message) => NumeralsOutcome::Error {
                            message,
                            inline: false,
                        },
                    }
                }
                Region::Inline(inline) => {
                    let continues = previous_inline_end.is_some_and(|end: usize| {
                        note.source
                            .get(end..inline.byte_range.start)
                            .is_some_and(|between| !has_blank_line(between))
                    });
                    previous_inline_end = Some(inline.byte_range.end);
                    match engine.inline(&inline.expression, inline.mode, continues) {
                        Ok((text, tex)) => NumeralsOutcome::Inline { text, tex },
                        Err(message) => NumeralsOutcome::Error {
                            message,
                            inline: true,
                        },
                    }
                }
            };
            NumeralsRegion {
                byte_range: region.range().clone(),
                line_number: region.line_number(),
                outcome,
            }
        })
        .collect()
}

/// Evaluate `source` (the note at `source_path`, when known) with its
/// scope: the note's metadata from `note_index`, or the frontmatter of an
/// unindexed source, and the metadata of the notes its `[[Note]].property`
/// references resolve to in `note_index`. A permission-scoped index keeps
/// unreadable notes out of reach.
#[must_use]
pub fn evaluate_numerals_in_source<S: std::hash::BuildHasher>(
    config: &crate::config::VaultConfig,
    note_index: Option<&std::collections::HashMap<String, crate::NoteRecord, S>>,
    source_path: Option<&str>,
    source: &str,
    parsed: &ParsedDocument,
) -> Vec<NumeralsRegion> {
    if !has_numerals(parsed, &config.numerals) {
        return Vec::new();
    }
    let note = source_path
        .zip(note_index)
        .and_then(|(path, index)| index.values().find(|note| note.document_path == path));
    let metadata = note.map_or_else(
        || {
            parsed
                .frontmatter
                .as_ref()
                .and_then(|frontmatter| serde_json::to_value(frontmatter).ok())
                .unwrap_or(Value::Null)
        },
        |note| note.properties.clone(),
    );
    let mut references = BTreeMap::new();
    if let Some(index) = note_index {
        for name in cross_note_reference_names(parsed) {
            let target = name.split(['|', '#']).next().unwrap_or_default();
            let resolved = crate::note_lookup::resolve_identity(
                || {
                    index.values().map(|note| crate::note_lookup::NoteIdentity {
                        path: &note.document_path,
                        file_name: &note.file_name,
                        aliases: &note.aliases,
                    })
                },
                |key| index.get(key).map(|note| note.document_path.as_str()),
                source_path.unwrap_or_default(),
                target,
            )
            .and_then(|path| index.values().find(|note| note.document_path == path));
            if let Some(target) = resolved {
                references.insert(
                    name,
                    serde_json::json!({
                        "path": target.document_path,
                        "metadata": target.properties,
                    }),
                );
            }
        }
    }
    evaluate_note_numerals(
        config,
        &NumeralsNote {
            source,
            parsed,
            metadata,
            references,
        },
    )
}

fn has_blank_line(text: &str) -> bool {
    let mut lines = text.split('\n');
    lines.next();
    let mut lines = lines.peekable();
    while let Some(line) = lines.next() {
        if lines.peek().is_some() && line.trim().is_empty() {
            return true;
        }
    }
    false
}

/// The `@[name::value]` rewrites the note's blocks compute: one
/// `(line_index, new_line)` per changed line, with zero-based line indexes
/// into `source`.
#[must_use]
pub fn numerals_insertion_edits(source: &str, regions: &[NumeralsRegion]) -> Vec<(usize, String)> {
    static INSERTION: OnceLock<Regex> = OnceLock::new();
    let insertion = INSERTION.get_or_init(|| {
        Regex::new(r"(@\s*\[)([^\]:]+)(::([^\]]*))?(\].*)$")
            .expect("insertion regex should compile")
    });
    let lines = source.split('\n').collect::<Vec<_>>();
    let mut edits = Vec::new();
    for region in regions {
        let NumeralsOutcome::Block { output, .. } = &region.outcome else {
            continue;
        };
        // `line_number` is the fence's one-based line; the block's first
        // source line follows it.
        for inserted in &output.insertions {
            let index = region.line_number + inserted.line;
            let Some(line) = lines.get(index) else {
                continue;
            };
            let (body, carriage_return) = line
                .strip_suffix('\r')
                .map_or((*line, ""), |body| (body, "\r"));
            let replaced = insertion.replace(body, |captures: &regex::Captures<'_>| {
                format!(
                    "{}{}::{}{}",
                    &captures[1], &captures[2], inserted.value, &captures[5]
                )
            });
            if replaced != body {
                edits.push((index, format!("{replaced}{carriage_return}")));
            }
        }
    }
    edits
}

/// A block as Markdown: a `text` code block of `input → result` lines, or a
/// display-math `aligned` environment for the TeX style.
#[must_use]
pub fn block_markdown(
    style: NumeralsStyle,
    separator: &str,
    output: &NumeralsBlockOutput,
) -> String {
    let mut rendered = if style == NumeralsStyle::Tex {
        tex_block(output)
    } else {
        plain_block(separator, output)
    };
    for warning in &output.warnings {
        rendered.push_str("\n\n> [!warning] Numerals\n> ");
        rendered.push_str(warning);
    }
    rendered
}

fn plain_block(separator: &str, output: &NumeralsBlockOutput) -> String {
    let width = output
        .lines
        .iter()
        .filter(|line| line.result.is_some())
        .map(|line| line.input.chars().count())
        .chain(
            output
                .error
                .iter()
                .map(|error| error.input.trim_end().chars().count()),
        )
        .max()
        .unwrap_or(0);
    let mut lines = output
        .lines
        .iter()
        .map(|line| {
            let comment = line.comment.as_deref().unwrap_or_default();
            match &line.result {
                Some(result) => {
                    let pad = width.saturating_sub(line.input.chars().count());
                    let mut text = format!("{}{}{separator}{result}", line.input, " ".repeat(pad));
                    if !comment.is_empty() {
                        text.push_str("  ");
                        text.push_str(comment);
                    }
                    text
                }
                None => format!("{}{comment}", line.input).trim_end().to_string(),
            }
        })
        .collect::<Vec<_>>();
    if let Some(error) = &output.error {
        let input = error.input.trim_end();
        let pad = width.saturating_sub(input.chars().count());
        lines.push(format!(
            "{input}{}{separator}{}: {}",
            " ".repeat(pad),
            error.name,
            error.message
        ));
    }
    let body = lines.join("\n");
    let fence = if body.contains("```") { "~~~~" } else { "```" };
    format!("{fence}text\n{body}\n{fence}")
}

fn tex_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '{' | '}' | '#' | '$' | '%' | '&' | '_' | '^' | '~'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    format!("\\text{{{escaped}}}")
}

fn tex_block(output: &NumeralsBlockOutput) -> String {
    let mut rows = output
        .lines
        .iter()
        .map(|line| {
            if let (Some(input), Some(result)) = (&line.tex_input, &line.tex_result) {
                return format!("{input} &\\to {result}");
            }
            let text = format!(
                "{}{}",
                line.input,
                line.comment.as_deref().unwrap_or_default()
            );
            if text.trim().is_empty() {
                "&".to_string()
            } else {
                format!("{} &", tex_text(text.trim_end()))
            }
        })
        .collect::<Vec<_>>();
    if let Some(error) = &output.error {
        rows.push(format!(
            "{} &\\to {}",
            tex_text(error.input.trim_end()),
            tex_text(&format!("{}: {}", error.name, error.message))
        ));
    }
    format!(
        "$$\n\\begin{{aligned}}\n{}\n\\end{{aligned}}\n$$",
        rows.join(" \\\\\n")
    )
}

/// An inline result as Markdown: the text, or inline math for TeX.
#[must_use]
pub fn inline_markdown(text: &str, tex: bool) -> String {
    if tex {
        format!("${text}$")
    } else {
        text.to_string()
    }
}

/// A region as Markdown: a block replacement (`true`) or inline text.
#[must_use]
pub fn outcome_markdown(outcome: &NumeralsOutcome) -> (bool, String) {
    match outcome {
        NumeralsOutcome::Block {
            style,
            separator,
            output,
        } => (true, block_markdown(*style, separator, output)),
        NumeralsOutcome::Inline { text, tex } => (false, inline_markdown(text, *tex)),
        NumeralsOutcome::Error {
            message,
            inline: true,
        } => (false, format!("*(Numerals error: {message})*")),
        NumeralsOutcome::Error {
            message,
            inline: false,
        } => {
            let mut callout = "> [!warning] Numerals error".to_string();
            for line in message.lines() {
                callout.push_str("\n> ");
                callout.push_str(line);
            }
            (true, callout)
        }
    }
}

/// The message shown where Numerals cannot evaluate.
#[cfg(not(feature = "js_runtime"))]
pub(crate) const RUNTIME_REQUIRED: &str =
    "Numerals calculations need Vulcan built with the `js_runtime` feature";

/// Loads a Numerals engine on this thread ahead of its first render when
/// the vault enables Numerals; later renders on the thread reuse it.
pub fn prewarm(config: &crate::config::VaultConfig) {
    if config.numerals.enabled {
        engine::prewarm(config);
    }
}

/// A mathjs engine for one render's Meta Bind math views, started on
/// first use; see [`crate::metabind`].
pub struct MathEvaluator {
    engine: engine::MathEngine,
}

impl MathEvaluator {
    #[must_use]
    pub fn start(config: &crate::config::VaultConfig) -> Self {
        Self {
            engine: engine::MathEngine::start(config),
        }
    }

    /// Evaluates `expression` with `scope`'s variables: the result as text
    /// and as a JSON value (a number where mathjs gives one).
    pub fn evaluate(&mut self, expression: &str, scope: &Value) -> Result<(String, Value), String> {
        self.engine.evaluate(expression, scope)
    }
}

#[cfg(feature = "js_runtime")]
mod engine {
    use std::cell::{Cell, RefCell};
    use std::time::{Duration, Instant};

    use rquickjs::{CatchResultExt, Context, Module, Runtime};

    use super::{InlineNumeralsMode, NumeralsBlockOutput, NumeralsNote, NumeralsStyle};
    use crate::config::{
        NumeralsCurrencyDisplay, NumeralsCurrencyPrecision, NumeralsNumberFormat, VaultConfig,
    };

    /// mathjs (with build.rs's speed patch) and `numerals_prelude.js` as one
    /// module, compiled to `QuickJS` bytecode by `build.rs`, and its source.
    const BYTECODE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/numerals.qjsbc"));
    const SOURCE: &str = include_str!(concat!(env!("OUT_DIR"), "/numerals.js"));
    /// mathjs is about 650 KB of source; loading it takes longer than a
    /// typical script, so its budget is separate from the per-call timeout.
    const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

    /// How many loaded engines a thread keeps for reuse: a render can hold
    /// a Numerals engine and a Meta Bind one at once.
    const IDLE_ENGINES: usize = 2;

    thread_local! {
        /// Loaded engines waiting for the next render on this thread, with
        /// the settings they were configured for. Loading mathjs is most of
        /// the cost of evaluating a note, so long-lived processes (the
        /// language server, the daemon) and renders of many notes pay it
        /// once per thread.
        static IDLE: RefCell<Vec<Engine>> = const { RefCell::new(Vec::new()) };
        /// Engines this thread has loaded, for tests of reuse.
        static LOADS: Cell<usize> = const { Cell::new(0) };
    }

    /// How many engines this thread has loaded.
    #[cfg(test)]
    pub(super) fn loads() -> usize {
        LOADS.with(Cell::get)
    }

    /// Whether the source compiled for a big-endian target writes the same
    /// bytes build.rs embedded, which is what lets a cross build's host-run
    /// build script embed bytecode any target loads.
    #[cfg(test)]
    pub(super) fn bytecode_is_target_independent() -> bool {
        let runtime = Runtime::new().expect("runtime");
        let context = Context::full(&runtime).expect("context");
        let written = context.with(|ctx| {
            Module::declare(ctx.clone(), "numerals", SOURCE)
                .and_then(|module| {
                    module.write(rquickjs::WriteOptions {
                        endianness: rquickjs::WriteOptionsEndianness::Big,
                        ..rquickjs::WriteOptions::default()
                    })
                })
                .expect("the engine source compiles")
        });
        written == BYTECODE
    }

    /// Whether build.rs's bytecode loads, rather than the source fallback.
    #[cfg(test)]
    pub(super) fn bytecode_loads() -> bool {
        let runtime = Runtime::new().expect("runtime");
        let context = Context::full(&runtime).expect("context");
        // SAFETY: as in `Engine::new`.
        context.with(|ctx| unsafe { Module::load(ctx.clone(), BYTECODE) }.is_ok())
    }

    struct Engine {
        runtime: Runtime,
        context: Context,
        timeout: Duration,
        /// The settings and limits the engine was started with.
        key: String,
        /// Set when a call was interrupted or ran out of memory, which can
        /// leave mathjs half-initialised.
        poisoned: Cell<bool>,
    }

    /// The note's engine, or why it could not start.
    pub(super) struct NoteEngine {
        engine: Result<Engine, String>,
    }

    impl NoteEngine {
        pub(super) fn start(config: &VaultConfig, note: &NumeralsNote<'_>) -> Self {
            Self {
                engine: Engine::acquire(config).and_then(|engine| {
                    let metadata =
                        serde_json::to_string(&note.metadata).map_err(|error| error.to_string())?;
                    let references = serde_json::to_string(&note.references)
                        .map_err(|error| error.to_string())?;
                    engine.call("__numerals.beginNote", &[metadata, references])?;
                    Ok(engine)
                }),
            }
        }

        pub(super) fn block(
            &mut self,
            source: &str,
            style: NumeralsStyle,
        ) -> Result<NumeralsBlockOutput, String> {
            let engine = self.engine.as_ref().map_err(Clone::clone)?;
            let style = match style {
                NumeralsStyle::Tex => "tex",
                NumeralsStyle::Plain | NumeralsStyle::Highlight => "plain",
            };
            let json = engine.call("__numerals.block", &[source.to_string(), style.to_string()])?;
            serde_json::from_str(&json).map_err(|error| error.to_string())
        }

        pub(super) fn inline(
            &mut self,
            expression: &str,
            mode: InlineNumeralsMode,
            continues: bool,
        ) -> Result<(String, bool), String> {
            #[derive(serde::Deserialize)]
            struct Inline {
                text: String,
                tex: bool,
            }
            let engine = self.engine.as_ref().map_err(Clone::clone)?;
            let (mode, style) = match mode {
                InlineNumeralsMode::Result => ("result", "plain"),
                InlineNumeralsMode::Equation => ("equation", "plain"),
                InlineNumeralsMode::TexResult => ("result", "tex"),
                InlineNumeralsMode::TexEquation => ("equation", "tex"),
            };
            let json = engine.call_with(
                "__numerals.inline",
                &[expression.to_string(), mode.to_string(), style.to_string()],
                Some(continues),
            )?;
            let inline: Inline = serde_json::from_str(&json).map_err(|error| error.to_string())?;
            Ok((inline.text, inline.tex))
        }
    }

    pub(super) fn prewarm(config: &VaultConfig) {
        if let Ok(engine) = Engine::acquire(config) {
            engine.release();
        }
    }

    /// mathjs alone, for Meta Bind views.
    pub(super) struct MathEngine {
        engine: Result<Engine, String>,
    }

    impl MathEngine {
        pub(super) fn start(config: &VaultConfig) -> Self {
            Self {
                engine: Engine::acquire(config),
            }
        }

        pub(super) fn evaluate(
            &mut self,
            expression: &str,
            scope: &serde_json::Value,
        ) -> Result<(String, serde_json::Value), String> {
            let engine = self.engine.as_ref().map_err(Clone::clone)?;
            let scope = serde_json::to_string(scope).map_err(|error| error.to_string())?;
            let json = engine.call(
                "(function (expression, scope) {\
                   var result = math.evaluate(expression, JSON.parse(scope));\
                   var text = typeof result === 'string' ? result : math.format(result, { precision: 14 });\
                   var value = typeof result === 'number' && isFinite(result) ? result\
                     : typeof result === 'boolean' ? result : text;\
                   return JSON.stringify({ text: text, value: value });\
                 })",
                &[expression.to_string(), scope],
            )?;
            let parsed: serde_json::Value =
                serde_json::from_str(&json).map_err(|error| error.to_string())?;
            Ok((
                parsed["text"].as_str().unwrap_or_default().to_string(),
                parsed["value"].clone(),
            ))
        }
    }

    impl Drop for NoteEngine {
        fn drop(&mut self) {
            if let Ok(engine) = std::mem::replace(&mut self.engine, Err(String::new())) {
                engine.release();
            }
        }
    }

    impl Drop for MathEngine {
        fn drop(&mut self) {
            if let Ok(engine) = std::mem::replace(&mut self.engine, Err(String::new())) {
                engine.release();
            }
        }
    }

    fn engine_key(config: &VaultConfig) -> String {
        let limits = &config.js_runtime;
        format!(
            "{}|{}|{}",
            limits.memory_limit_mb,
            limits.stack_limit_kb,
            settings_json(config)
        )
    }

    fn timeout(config: &VaultConfig) -> Duration {
        Duration::from_secs(
            u64::try_from(config.js_runtime.default_timeout_seconds.max(1)).unwrap_or(u64::MAX),
        )
    }

    fn settings_json(config: &VaultConfig) -> String {
        let numerals = &config.numerals;
        let number_format = match numerals.number_format {
            NumeralsNumberFormat::System => "system",
            NumeralsNumberFormat::Fixed => "fixed",
            NumeralsNumberFormat::Exponential => "exponential",
            NumeralsNumberFormat::Engineering => "engineering",
            NumeralsNumberFormat::CommaPeriod => "comma-period",
            NumeralsNumberFormat::PeriodComma => "period-comma",
            NumeralsNumberFormat::SpaceComma => "space-comma",
            NumeralsNumberFormat::Indian => "indian",
        };
        let custom = (!numerals.custom_currency_symbol.is_empty()
            && !numerals.custom_currency_code.is_empty())
        .then(|| {
            serde_json::json!({
                "symbol": numerals.custom_currency_symbol,
                "currency": numerals.custom_currency_code,
            })
        });
        serde_json::json!({
            "number_format": number_format,
            "currency_precision": match numerals.currency_precision {
                NumeralsCurrencyPrecision::CurrencyStandard => "currency-standard",
                NumeralsCurrencyPrecision::FollowNumberFormat => "follow-number-format",
            },
            "currency_display": match numerals.currency_display {
                NumeralsCurrencyDisplay::Code => "code",
                NumeralsCurrencyDisplay::Symbol => "symbol",
            },
            "dollar_currency": numerals.dollar_currency,
            "yen_currency": numerals.yen_currency,
            "custom_currency": custom,
            "currency_decimal_places": numerals.custom_currency_decimal_places.min(20),
            "force_all": numerals.force_all_frontmatter,
            "hide_emitter_markup": numerals.hide_emitter_markup,
            "hide_non_emitters": numerals.hide_non_emitters,
            "equation_separator": numerals.inline_equation_separator,
            "cross_note_references": numerals.cross_note_references,
        })
        .to_string()
    }

    impl Engine {
        /// A loaded engine for `config`: an idle one of this thread's, or a
        /// new one.
        fn acquire(config: &VaultConfig) -> Result<Self, String> {
            let key = engine_key(config);
            let idle = IDLE
                .try_with(|idle| {
                    let mut idle = idle.borrow_mut();
                    let position = idle.iter().position(|engine| engine.key == key)?;
                    Some(idle.swap_remove(position))
                })
                .ok()
                .flatten();
            match idle {
                Some(mut engine) => {
                    engine.timeout = timeout(config);
                    Ok(engine)
                }
                None => Self::new(config, key),
            }
        }

        /// Keeps the engine for the next render on this thread, unless a
        /// call left it changed: an interrupted call, or an expression that
        /// changed mathjs itself (`createUnit`, `config`, `import`).
        fn release(self) {
            if self.poisoned.get() || self.call("__numerals.pristine", &[]).as_deref() != Ok("true")
            {
                return;
            }
            let _ = IDLE.try_with(|idle| {
                let mut idle = idle.borrow_mut();
                if idle.len() >= IDLE_ENGINES {
                    idle.remove(0);
                }
                idle.push(self);
            });
        }

        fn new(config: &VaultConfig, key: String) -> Result<Self, String> {
            LOADS.with(|loads| loads.set(loads.get() + 1));
            let limits = &config.js_runtime;
            let runtime = Runtime::new().map_err(|error| error.to_string())?;
            // mathjs needs room beyond a script's: at least 64 MB.
            runtime.set_memory_limit(limits.memory_limit_mb.max(64).saturating_mul(1024 * 1024));
            runtime.set_max_stack_size(limits.stack_limit_kb.saturating_mul(1024));
            let context = Context::full(&runtime).map_err(|error| error.to_string())?;
            let started = Instant::now();
            runtime
                .set_interrupt_handler(Some(Box::new(move || started.elapsed() >= LOAD_TIMEOUT)));
            let loaded = context.with(|ctx| -> Result<(), String> {
                // SAFETY: `BYTECODE` was written by build.rs with the QuickJS
                // this crate links (Cargo.lock pins one rquickjs-sys), whose
                // bytecode does not depend on the target (see build.rs);
                // QuickJS checks the bytecode version and the source is
                // compiled instead when loading fails.
                let module = match unsafe { Module::load(ctx.clone(), BYTECODE) } {
                    Ok(module) => module,
                    Err(_) => Module::declare(ctx.clone(), "numerals", SOURCE)
                        .catch(&ctx)
                        .map_err(|error| format!("mathjs failed to load: {error}"))?,
                };
                let (_, promise) = module
                    .eval()
                    .catch(&ctx)
                    .map_err(|error| format!("mathjs failed to load: {error}"))?;
                promise
                    .finish::<()>()
                    .catch(&ctx)
                    .map_err(|error| format!("mathjs failed to load: {error}"))
            });
            runtime.set_interrupt_handler(None);
            loaded?;
            let engine = Self {
                runtime,
                context,
                timeout: timeout(config),
                key,
                poisoned: Cell::new(false),
            };
            engine.call("__numerals.configure", &[settings_json(config)])?;
            Ok(engine)
        }

        fn call(&self, function: &str, args: &[String]) -> Result<String, String> {
            self.call_with(function, args, None)
        }

        /// Calls `function` with string arguments and an optional trailing
        /// boolean; it returns a string (JSON) or nothing.
        fn call_with(
            &self,
            function: &str,
            args: &[String],
            flag: Option<bool>,
        ) -> Result<String, String> {
            let mut rendered = args
                .iter()
                .map(|arg| serde_json::to_string(arg).map_err(|error| error.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(flag) = flag {
                rendered.push(flag.to_string());
            }
            let started = Instant::now();
            let timeout = self.timeout;
            self.runtime
                .set_interrupt_handler(Some(Box::new(move || started.elapsed() >= timeout)));
            let result = self.context.with(|ctx| -> Result<String, String> {
                ctx.eval::<Option<String>, _>(format!("{function}({})", rendered.join(", ")))
                    .catch(&ctx)
                    .map(Option::unwrap_or_default)
                    .map_err(|error| {
                        let message = error.to_string();
                        if ["interrupted", "out of memory", "stack overflow"]
                            .iter()
                            .any(|fatal| message.contains(fatal))
                        {
                            self.poisoned.set(true);
                        }
                        message
                            .strip_prefix("Error: ")
                            .unwrap_or(&message)
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .to_string()
                    })
            });
            self.runtime.set_interrupt_handler(None);
            result
        }
    }
}

#[cfg(not(feature = "js_runtime"))]
mod engine {
    use super::{InlineNumeralsMode, NumeralsBlockOutput, NumeralsNote, NumeralsStyle};

    pub(super) struct NoteEngine;

    impl NoteEngine {
        pub(super) fn start(
            _config: &crate::config::VaultConfig,
            _note: &NumeralsNote<'_>,
        ) -> Self {
            Self
        }

        #[allow(clippy::unused_self)]
        pub(super) fn block(
            &mut self,
            _source: &str,
            _style: NumeralsStyle,
        ) -> Result<NumeralsBlockOutput, String> {
            Err(super::RUNTIME_REQUIRED.to_string())
        }

        #[allow(clippy::unused_self)]
        pub(super) fn inline(
            &mut self,
            _expression: &str,
            _mode: InlineNumeralsMode,
            _continues: bool,
        ) -> Result<(String, bool), String> {
            Err(super::RUNTIME_REQUIRED.to_string())
        }
    }

    pub(super) fn prewarm(_config: &crate::config::VaultConfig) {}

    pub(super) struct MathEngine;

    impl MathEngine {
        pub(super) fn start(_config: &crate::config::VaultConfig) -> Self {
            Self
        }

        #[allow(clippy::unused_self)]
        pub(super) fn evaluate(
            &mut self,
            _expression: &str,
            _scope: &serde_json::Value,
        ) -> Result<(String, serde_json::Value), String> {
            Err(super::RUNTIME_REQUIRED.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "js_runtime")]
    fn evaluate(config: &crate::config::VaultConfig, source: &str) -> Vec<String> {
        let parsed = crate::parse_document(source, config);
        evaluate_numerals_in_source::<std::collections::hash_map::RandomState>(
            config, None, None, source, &parsed,
        )
        .iter()
        .map(|region| outcome_markdown(&region.outcome).1)
        .collect()
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn engines_load_the_precompiled_bytecode() {
        assert!(engine::bytecode_loads());
        assert!(
            engine::bytecode_is_target_independent(),
            "QuickJS bytecode now depends on byte order; build.rs must write it for the target"
        );
    }

    #[cfg(feature = "js_runtime")]
    #[test]
    fn engines_are_reused_until_an_expression_changes_mathjs() {
        let mut config = crate::config::VaultConfig::default();
        config.numerals.enabled = true;
        let loads = engine::loads();
        prewarm(&config);
        assert_eq!(engine::loads(), loads + 1, "prewarming loads an engine");
        assert_eq!(evaluate(&config, "`#: 2 + 2`\n"), ["4"]);
        assert_eq!(evaluate(&config, "`#: $x = 3`\n"), ["3"]);
        assert_eq!(
            engine::loads(),
            loads + 1,
            "the second note reuses the engine"
        );
        assert!(
            evaluate(&config, "`#: $x + 1`\n")[0].contains("Undefined symbol"),
            "note state does not leak into the next note"
        );

        assert_eq!(
            evaluate(&config, "`#: createUnit(\"zorp\", \"3 m\")`\n").len(),
            1
        );
        assert!(
            evaluate(&config, "`#: 2 zorp to m`\n")[0].contains("zorp"),
            "a unit one note created is gone in the next"
        );
        assert_eq!(
            engine::loads(),
            loads + 2,
            "an engine changed by a note is not reused"
        );

        config.numerals.dollar_currency = "CAD".to_string();
        evaluate(&config, "`#: 1 + 1`\n");
        assert_eq!(
            engine::loads(),
            loads + 3,
            "other settings load their own engine"
        );
        config.numerals.dollar_currency = "USD".to_string();
        evaluate(&config, "`#: 1 + 1`\n");
        assert_eq!(engine::loads(), loads + 3, "and the first one is kept");
    }

    #[test]
    fn styles_follow_the_fence_language_then_the_config() {
        let mut config = NumeralsConfig::default();
        assert_eq!(block_style("math", &config), NumeralsStyle::Plain);
        assert_eq!(block_style("math-TeX", &config), NumeralsStyle::Tex);
        config.render_style = NumeralsRenderStyle::Tex;
        assert_eq!(block_style("Math", &config), NumeralsStyle::Tex);
        assert_eq!(block_style("math-plain", &config), NumeralsStyle::Plain);
        assert_eq!(
            block_style("math-highlight", &config),
            NumeralsStyle::Highlight
        );
    }

    #[test]
    fn plain_blocks_align_results_and_show_errors() {
        let output = NumeralsBlockOutput {
            lines: vec![
                NumeralsLine {
                    line: 0,
                    input: "a = 1".to_string(),
                    comment: Some("# first".to_string()),
                    emitter: false,
                    result: Some("1".to_string()),
                    tex_input: None,
                    tex_result: None,
                },
                NumeralsLine {
                    line: 1,
                    input: "long_name = a + 1".to_string(),
                    comment: None,
                    emitter: false,
                    result: Some("2".to_string()),
                    tex_input: None,
                    tex_result: None,
                },
                NumeralsLine {
                    line: 2,
                    input: String::new(),
                    comment: Some("# note".to_string()),
                    emitter: false,
                    result: None,
                    tex_input: None,
                    tex_result: None,
                },
            ],
            error: Some(NumeralsLineError {
                name: "SyntaxError".to_string(),
                message: "Unexpected end".to_string(),
                input: "b +".to_string(),
            }),
            warnings: vec!["Frontmatter: bad".to_string()],
            insertions: Vec::new(),
            emitters: false,
        };
        assert_eq!(
            block_markdown(NumeralsStyle::Plain, " → ", &output),
            concat!(
                "```text\n",
                "a = 1             → 1  # first\n",
                "long_name = a + 1 → 2\n",
                "# note\n",
                "b +               → SyntaxError: Unexpected end\n",
                "```\n\n",
                "> [!warning] Numerals\n> Frontmatter: bad",
            )
        );
    }

    #[test]
    fn insertion_edits_rewrite_only_changed_lines() {
        let source = "intro\n```math\nx = 2\n@[total::0] = x * 3\n@[same::4] = 4\n```\n";
        let region = NumeralsRegion {
            byte_range: 6..source.len(),
            line_number: 2,
            outcome: NumeralsOutcome::Block {
                style: NumeralsStyle::Plain,
                separator: " → ".to_string(),
                output: NumeralsBlockOutput {
                    lines: Vec::new(),
                    error: None,
                    warnings: Vec::new(),
                    insertions: vec![
                        NumeralsInsertion {
                            line: 1,
                            value: "6".to_string(),
                        },
                        NumeralsInsertion {
                            line: 2,
                            value: "4".to_string(),
                        },
                    ],
                    emitters: false,
                },
            },
        };
        assert_eq!(
            numerals_insertion_edits(source, &[region]),
            vec![(3, "@[total::6] = x * 3".to_string())]
        );
    }

    #[test]
    fn blank_lines_end_an_inline_section() {
        assert!(!has_blank_line(" and "));
        assert!(!has_blank_line(" end\nnext "));
        assert!(has_blank_line(" end\n\nnext "));
        assert!(has_blank_line(" end\n  \nnext "));
    }
}
