//! Meta Bind-compatible input fields, views, and buttons (`INPUT[...]`,
//! `VIEW[...]`, `BUTTON[...]`, and the `meta-bind*` blocks), rendered from
//! the note's metadata. Rendering only reads; [`plan_set`] and
//! [`view_write_backs`] compute the frontmatter edits `vulcan metabind set`
//! and `vulcan metabind apply` write. The contract is
//! `docs/specs/live-documents.md` (9.42.3).
//!
//! This is a reimplementation from Meta Bind's documented syntax and
//! behaviour. The plugin is GPL-3.0, and none of its code is used here.

use crate::config::{MetaBindConfig, VaultConfig};
use crate::parser::types::{MetaBindSourceKind, ParsedDocument};
use crate::{NoteRecord, VaultPaths};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;

pub mod buttons;

// ---------------------------------------------------------------- targets

/// Where a bind target's value lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaBindStorage {
    Frontmatter,
    /// `memory^`: per note, never persisted.
    Memory,
    /// `globalMemory^`: shared by all notes, never persisted.
    GlobalMemory,
    /// `scope^`: only inside an API scope, which Vulcan has none of.
    Scope,
}

/// One step of a property path: `a`, `["a b"]`, or `[0]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

/// A parsed bind target, such as `Other Note#stats.hp` or `memory^count`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindTarget {
    pub storage: MetaBindStorage,
    /// The note named before `#`; `None` is the note holding the field.
    pub file: Option<String>,
    pub path: Vec<PathSegment>,
}

impl BindTarget {
    /// Parses a bind target as Meta Bind writes them.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty bind target".to_string());
        }
        let mut storage = MetaBindStorage::Frontmatter;
        let mut rest = text;
        if let Some(caret) = find_outside_brackets(rest, '^') {
            storage = match rest[..caret].trim() {
                "memory" => MetaBindStorage::Memory,
                "globalMemory" => MetaBindStorage::GlobalMemory,
                "scope" => MetaBindStorage::Scope,
                "frontmatter" => MetaBindStorage::Frontmatter,
                other => return Err(format!("unknown storage type `{other}`")),
            };
            rest = &rest[caret + 1..];
        }
        let mut file = None;
        if let Some(hash) = find_outside_brackets(rest, '#') {
            let name = rest[..hash].trim();
            if name.is_empty() || name.contains(['{', '}', '[', ']', '|', ':', '?']) {
                return Err(format!("invalid file name `{name}` in bind target"));
            }
            file = Some(name.to_string());
            rest = &rest[hash + 1..];
        }
        let path = parse_property_path(rest.trim())?;
        Ok(Self {
            storage,
            file,
            path,
        })
    }

    /// The top-level property the target lives under.
    #[must_use]
    pub fn root_key(&self) -> Option<&str> {
        match self.path.first() {
            Some(PathSegment::Key(key)) => Some(key),
            _ => None,
        }
    }
}

impl std::fmt::Display for BindTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.storage {
            MetaBindStorage::Frontmatter => {}
            MetaBindStorage::Memory => formatter.write_str("memory^")?,
            MetaBindStorage::GlobalMemory => formatter.write_str("globalMemory^")?,
            MetaBindStorage::Scope => formatter.write_str("scope^")?,
        }
        if let Some(file) = &self.file {
            write!(formatter, "{file}#")?;
        }
        for (index, segment) in self.path.iter().enumerate() {
            match segment {
                PathSegment::Key(key) if is_plain_identifier(key) => {
                    if index > 0 {
                        formatter.write_str(".")?;
                    }
                    formatter.write_str(key)?;
                }
                PathSegment::Key(key) => write!(formatter, "[{}]", Value::String(key.clone()))?,
                PathSegment::Index(position) => write!(formatter, "[{position}]")?,
            }
        }
        Ok(())
    }
}

fn find_outside_brackets(text: &str, wanted: char) -> Option<usize> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            _ if character == wanted && depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn is_identifier_start(character: char) -> bool {
    character.is_alphabetic() || character == '_' || character == '$'
}

fn is_identifier_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '$' | '-')
}

fn is_plain_identifier(text: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(is_identifier_start) && characters.all(is_identifier_char)
}

fn parse_property_path(text: &str) -> Result<Vec<PathSegment>, String> {
    let characters = text.chars().collect::<Vec<_>>();
    let mut position = 0;
    let mut path = Vec::new();
    let mut expect_identifier = true;
    while position < characters.len() {
        let character = characters[position];
        if character == '[' {
            position += 1;
            if characters.get(position) == Some(&'"') {
                position += 1;
                let mut key = String::new();
                loop {
                    match characters.get(position) {
                        None => return Err(format!("unclosed string in `{text}`")),
                        Some('\\') => {
                            if let Some(next) = characters.get(position + 1) {
                                key.push(*next);
                            }
                            position += 2;
                        }
                        Some('"') => {
                            position += 1;
                            break;
                        }
                        Some(other) => {
                            key.push(*other);
                            position += 1;
                        }
                    }
                }
                path.push(PathSegment::Key(key));
            } else {
                let start = position;
                while characters.get(position).is_some_and(char::is_ascii_digit) {
                    position += 1;
                }
                if start == position {
                    return Err(format!("expected an index or a quoted key in `{text}`"));
                }
                let digits = characters[start..position].iter().collect::<String>();
                path.push(PathSegment::Index(
                    digits
                        .parse()
                        .map_err(|_| format!("bad index in `{text}`"))?,
                ));
            }
            if characters.get(position) != Some(&']') {
                return Err(format!("expected `]` in `{text}`"));
            }
            position += 1;
            expect_identifier = false;
        } else if character == '.' && !path.is_empty() && !expect_identifier {
            position += 1;
            expect_identifier = true;
        } else if expect_identifier && is_identifier_start(character) {
            let start = position;
            while characters
                .get(position)
                .is_some_and(|character| is_identifier_char(*character))
            {
                position += 1;
            }
            path.push(PathSegment::Key(
                characters[start..position].iter().collect(),
            ));
            expect_identifier = false;
        } else {
            return Err(format!(
                "unexpected `{character}` in property path `{text}` (write names with spaces as [\"a b\"])"
            ));
        }
    }
    if path.is_empty() || expect_identifier {
        return Err(format!("incomplete property path `{text}`"));
    }
    Ok(path)
}

// ----------------------------------------------------------- declarations

/// One `name(value, ...)` argument, with its values as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MetaBindArgument {
    pub name: String,
    pub values: Vec<String>,
}

impl MetaBindArgument {
    fn literal(&self, index: usize) -> Option<Value> {
        self.values.get(index).map(|value| parse_literal(value))
    }
}

/// An argument value as Meta Bind reads literals: `null`, `true`/`false`,
/// a number, or a string.
#[must_use]
pub fn parse_literal(text: &str) -> Value {
    if text.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    match text {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    let bytes = text.strip_prefix('-').unwrap_or(text);
    let mut parts = bytes.splitn(2, '.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    let numeric = !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        });
    if numeric {
        if let Ok(number) = text.parse::<i64>() {
            return Value::from(number);
        }
        if let Some(number) = text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
        {
            return Value::Number(number);
        }
    }
    Value::String(text.to_string())
}

/// An `INPUT[...]` declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputDeclaration {
    pub input_type: String,
    pub arguments: Vec<MetaBindArgument>,
    pub target: Option<BindTarget>,
    /// The template the declaration started from, if any.
    pub template: Option<String>,
}

/// One part of a `VIEW[...]` template.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewPart {
    Text { text: String },
    Target { target: BindTarget },
}

/// A `VIEW[...][...]` declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewDeclaration {
    pub parts: Vec<ViewPart>,
    pub view_type: String,
    pub arguments: Vec<MetaBindArgument>,
    /// Where the view writes its result (`VIEW[...][math:target]`).
    pub write_target: Option<BindTarget>,
}

/// A parsed field declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetaBindDeclaration {
    Input(InputDeclaration),
    View(ViewDeclaration),
    /// `BUTTON[id, ...]`: references to buttons declared elsewhere.
    Buttons {
        ids: Vec<String>,
    },
    /// A `meta-bind-button` block.
    Button {
        button: Value,
    },
    /// A `meta-bind-js-view` block.
    JsView(JsViewDeclaration),
    /// A `meta-bind-embed` block.
    Embed {
        target: String,
    },
}

/// A `meta-bind-js-view` block: `{target} as name` bindings (also
/// `{target} and children as name`), an optional `save to {target}` and
/// `hidden`, then `---` and the code.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JsViewDeclaration {
    pub bindings: Vec<(String, BindTarget)>,
    pub save_to: Option<BindTarget>,
    pub hidden: bool,
    pub code: String,
}

/// Parses a `meta-bind-js-view` block's text.
pub fn parse_js_view(text: &str) -> Result<JsViewDeclaration, String> {
    let mut lines = text.split_inclusive('\n');
    let mut declaration = JsViewDeclaration {
        bindings: Vec::new(),
        save_to: None,
        hidden: false,
        code: String::new(),
    };
    let braced = |text: &str| -> Result<BindTarget, String> {
        let inner = text
            .trim()
            .strip_prefix('{')
            .and_then(|rest| rest.strip_suffix('}'))
            .ok_or_else(|| format!("expected `{{target}}`, not `{}`", text.trim()))?;
        BindTarget::parse(inner.trim())
    };
    let mut separated = false;
    for line in lines.by_ref() {
        let line = line.trim();
        if line == "---" {
            separated = true;
            break;
        }
        if line.is_empty() {
            continue;
        }
        if line == "hidden" {
            declaration.hidden = true;
        } else if let Some(target) = line.strip_prefix("save to ") {
            declaration.save_to = Some(braced(target)?);
        } else if let Some((target, name)) = line.rsplit_once(" as ") {
            let target = target.trim();
            let target = target.strip_suffix(" and children").unwrap_or(target);
            let name = name.trim();
            if !is_plain_identifier(name) {
                return Err(format!("`{name}` is not a variable name"));
            }
            declaration
                .bindings
                .push((name.to_string(), braced(target)?));
        } else {
            return Err(format!(
                "expected `{{target}} as name`, `save to {{target}}`, or `hidden`, not `{line}`"
            ));
        }
    }
    if !separated {
        return Err("a JS view needs a `---` line between its bindings and its code".to_string());
    }
    declaration.code = lines.collect();
    Ok(declaration)
}

/// Runs `meta-bind-js-view` code for a render, under the render's
/// `DataviewJS` policy.
pub trait MetaBindScripts {
    /// Runs `code` (an async function body) with `context` as its `context`
    /// constant and Meta Bind's `engine.markdown.create`. `None` when the
    /// render turns `DataviewJS` off.
    fn run_js_view(
        &self,
        code: &str,
        context: &Value,
    ) -> Option<Result<MetaBindScriptValue, String>>;
}

/// What a script returned.
#[derive(Debug, Clone, PartialEq)]
pub enum MetaBindScriptValue {
    /// `engine.markdown.create(text)`.
    Markdown(String),
    Value(Value),
}

/// The JavaScript that runs `code` with `context` and Meta Bind's
/// `engine.markdown.create`, for [`crate::DataviewJsSession::evaluate_async_function`].
#[must_use]
pub fn script_body(code: &str, context: &Value) -> String {
    format!(
        "const context = {context};\n\
         const engine = {{ markdown: {{ create: (text) => ({{ __vulcanMarkdown: String(text) }}) }}, \
         getPlugin: () => undefined }};\n\
         return await (async () => {{\n{code}\n}})();"
    )
}

/// A script's returned JSON as Meta Bind reads it.
#[must_use]
pub fn script_value(value: Option<Value>) -> MetaBindScriptValue {
    match value {
        Some(Value::Object(object))
            if object.len() == 1 && object.contains_key("__vulcanMarkdown") =>
        {
            MetaBindScriptValue::Markdown(
                object["__vulcanMarkdown"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        value => MetaBindScriptValue::Value(value.unwrap_or(Value::Null)),
    }
}

/// [`MetaBindScripts`] over a `DataviewJS` session, started on first use.
pub struct DataviewJsScripts<'a> {
    paths: &'a VaultPaths,
    source_path: Option<&'a str>,
    options: Option<crate::DataviewJsEvalOptions>,
    session: std::cell::RefCell<Option<Result<crate::DataviewJsSession, String>>>,
}

impl<'a> DataviewJsScripts<'a> {
    /// Scripts that run with `options`, or not at all for `None` (the
    /// render's policy is `off`).
    #[must_use]
    pub fn new(
        paths: &'a VaultPaths,
        source_path: Option<&'a str>,
        options: Option<crate::DataviewJsEvalOptions>,
    ) -> Self {
        Self {
            paths,
            source_path,
            options,
            session: std::cell::RefCell::new(None),
        }
    }
}

impl MetaBindScripts for DataviewJsScripts<'_> {
    fn run_js_view(
        &self,
        code: &str,
        context: &Value,
    ) -> Option<Result<MetaBindScriptValue, String>> {
        let options = self.options.clone()?;
        let mut session = self.session.borrow_mut();
        let session = session.get_or_insert_with(|| {
            crate::DataviewJsSession::new(self.paths, self.source_path, options)
                .map_err(|error| error.to_string())
        });
        Some(match session {
            Ok(session) => session
                .evaluate_async_function(&script_body(code, context))
                .map(script_value)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.clone()),
        })
    }
}

struct Cursor<'a> {
    text: &'a str,
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, position: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.position..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.position += character.len_utf8();
        Some(character)
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn eat(&mut self, wanted: char) -> bool {
        if self.peek() == Some(wanted) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn identifier(&mut self) -> Option<&'a str> {
        let start = self.position;
        if !self.peek().is_some_and(is_identifier_start) {
            return None;
        }
        while self.peek().is_some_and(is_identifier_char) {
            self.bump();
        }
        Some(&self.text[start..self.position])
    }

    fn arguments(&mut self) -> Result<Vec<MetaBindArgument>, String> {
        let mut arguments = Vec::new();
        loop {
            self.skip_ws();
            let name = self
                .identifier()
                .ok_or_else(|| format!("expected an argument name at `{}`", self.rest()))?
                .to_string();
            self.skip_ws();
            let mut values = Vec::new();
            if self.eat('(') {
                loop {
                    self.skip_ws();
                    values.push(self.argument_value()?);
                    self.skip_ws();
                    if self.eat(',') {
                        continue;
                    }
                    if self.eat(')') {
                        break;
                    }
                    return Err(format!("expected `,` or `)` in argument `{name}`"));
                }
            }
            arguments.push(MetaBindArgument { name, values });
            self.skip_ws();
            if !self.eat(',') {
                return Ok(arguments);
            }
        }
    }

    fn argument_value(&mut self) -> Result<String, String> {
        if self.eat('\'') {
            let mut value = String::new();
            loop {
                match self.bump() {
                    None => return Err("unclosed quoted argument value".to_string()),
                    Some('\\') => match self.bump() {
                        Some('\'') => value.push('\''),
                        Some('\\') => value.push('\\'),
                        Some(other) => {
                            value.push('\\');
                            value.push(other);
                        }
                        None => return Err("unclosed quoted argument value".to_string()),
                    },
                    Some('\'') => return Ok(value),
                    Some(other) => value.push(other),
                }
            }
        }
        let start = self.position;
        while self
            .peek()
            .is_some_and(|character| !matches!(character, '(' | ')' | '\'' | ','))
        {
            self.bump();
        }
        let value = self.text[start..self.position].trim();
        if value.is_empty() {
            return Err("empty argument value".to_string());
        }
        Ok(value.to_string())
    }
}

/// Parses inline code text or a `meta-bind` block body.
pub fn parse_declaration(
    text: &str,
    config: &MetaBindConfig,
) -> Result<MetaBindDeclaration, String> {
    let text = text.trim();
    if let Some(inner) = text
        .strip_prefix("INPUT[")
        .and_then(|t| t.strip_suffix(']'))
    {
        return parse_input(inner, config).map(MetaBindDeclaration::Input);
    }
    if let Some(inner) = text.strip_prefix("VIEW[").and_then(|t| t.strip_suffix(']')) {
        return parse_view(inner).map(MetaBindDeclaration::View);
    }
    if let Some(inner) = text
        .strip_prefix("BUTTON[")
        .and_then(|t| t.strip_suffix(']'))
    {
        let ids = inner
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Err("BUTTON[] names no button".to_string());
        }
        return Ok(MetaBindDeclaration::Buttons { ids });
    }
    Err("a Meta Bind field starts with INPUT[, VIEW[, or BUTTON[ and ends with ]".to_string())
}

/// The type, arguments, and target of an input or view options part.
type InputBody = (Option<String>, Vec<MetaBindArgument>, Option<BindTarget>);

/// The `type(args):target` part of an input, each piece optional.
fn parse_input_body(cursor: &mut Cursor<'_>) -> Result<InputBody, String> {
    cursor.skip_ws();
    let input_type = cursor.identifier().map(ToOwned::to_owned);
    cursor.skip_ws();
    let mut arguments = Vec::new();
    if cursor.eat('(') {
        arguments = cursor.arguments()?;
        cursor.skip_ws();
        if !cursor.eat(')') {
            return Err("expected `)` after the arguments".to_string());
        }
        cursor.skip_ws();
    }
    let mut target = None;
    if cursor.eat(':') {
        target = Some(BindTarget::parse(cursor.rest())?);
        cursor.position = cursor.text.len();
    }
    cursor.skip_ws();
    if !cursor.rest().is_empty() {
        return Err(format!("unexpected `{}`", cursor.rest()));
    }
    Ok((input_type, arguments, target))
}

fn parse_input(inner: &str, config: &MetaBindConfig) -> Result<InputDeclaration, String> {
    // `INPUT[template][overrides]`: the inner text holds `][`.
    let mut cursor = Cursor::new(inner);
    cursor.skip_ws();
    let start = cursor.position;
    let first = cursor.identifier();
    cursor.skip_ws();
    if let Some(name) = first {
        if cursor.rest().starts_with("][") {
            let overrides = &cursor.rest()[2..];
            let template = config
                .input_field_templates
                .iter()
                .find(|template| template.name == name)
                .ok_or_else(|| format!("unknown input field template `{name}`"))?;
            let declaration = template.declaration.trim();
            let body = declaration
                .strip_prefix("INPUT[")
                .and_then(|text| text.strip_suffix(']'))
                .unwrap_or(declaration);
            let (base_type, mut arguments, base_target) = parse_input_body(&mut Cursor::new(body))?;
            let (input_type, extra, target) = parse_input_body(&mut Cursor::new(overrides))?;
            for argument in extra {
                arguments.retain(|existing| existing.name != argument.name);
                arguments.push(argument);
            }
            return Ok(InputDeclaration {
                input_type: input_type
                    .or(base_type)
                    .ok_or_else(|| format!("template `{name}` names no input type"))?,
                arguments,
                target: target.or(base_target),
                template: Some(name.to_string()),
            });
        }
    }
    cursor.position = start;
    let (input_type, arguments, target) = parse_input_body(&mut cursor)?;
    Ok(InputDeclaration {
        input_type: input_type.ok_or_else(|| "INPUT[] names no input type".to_string())?,
        arguments,
        target,
        template: None,
    })
}

fn parse_view(inner: &str) -> Result<ViewDeclaration, String> {
    // The template runs to the first `]` outside braces and escapes.
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut characters = inner.char_indices().peekable();
    let mut template_end = inner.len();
    while let Some((index, character)) = characters.next() {
        match character {
            '\\' => {
                if let Some((_, next)) = characters.next() {
                    text.push(next);
                }
            }
            '{' => {
                let start = index + 1;
                let mut end = None;
                let mut in_string = false;
                for (position, inner_character) in characters.by_ref() {
                    match inner_character {
                        '"' => in_string = !in_string,
                        '}' if !in_string => {
                            end = Some(position);
                            break;
                        }
                        _ => {}
                    }
                }
                let end = end.ok_or_else(|| "unclosed `{` in VIEW template".to_string())?;
                if !text.is_empty() {
                    parts.push(ViewPart::Text {
                        text: std::mem::take(&mut text),
                    });
                }
                parts.push(ViewPart::Target {
                    target: BindTarget::parse(&inner[start..end])?,
                });
            }
            ']' => {
                template_end = index;
                break;
            }
            other => text.push(other),
        }
    }
    if !text.is_empty() {
        parts.push(ViewPart::Text { text });
    }
    let mut view_type = "math".to_string();
    let mut arguments = Vec::new();
    let mut write_target = None;
    if template_end < inner.len() {
        let options = inner[template_end + 1..].trim_start();
        let options = options
            .strip_prefix('[')
            .ok_or_else(|| "expected `[` after the VIEW template".to_string())?;
        let mut cursor = Cursor::new(options);
        let (kind, args, target) = parse_input_body(&mut cursor)?;
        if let Some(kind) = kind {
            view_type = kind;
        }
        arguments = args;
        write_target = target;
    }
    if !parts
        .iter()
        .any(|part| matches!(part, ViewPart::Target { .. }))
        && view_type != "math"
    {
        return Err("a VIEW names no {bind target}".to_string());
    }
    Ok(ViewDeclaration {
        parts,
        view_type,
        arguments,
        write_target,
    })
}

// ------------------------------------------------------------ input types

/// The kind of value an input type stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueKind {
    Toggle,
    Range,
    Text,
    Number,
    Single,
    Multiple,
    Date,
    Time,
    DateTime,
}

struct InputType {
    kind: ValueKind,
    block_only: bool,
    arguments: &'static [&'static str],
}

const COMMON_ARGUMENTS: &[&str] = &["class", "defaultValue", "title", "showcase"];

fn input_type(name: &str) -> Option<InputType> {
    let (kind, block_only, arguments): (ValueKind, bool, &'static [&'static str]) = match name {
        "toggle" => (ValueKind::Toggle, false, &["onValue", "offValue"]),
        "slider" => (
            ValueKind::Range,
            false,
            &["minValue", "maxValue", "stepSize", "addLabels"],
        ),
        "progressBar" => (
            ValueKind::Range,
            true,
            &["minValue", "maxValue", "stepSize"],
        ),
        "text" | "textArea" => (ValueKind::Text, false, &["placeholder", "limit"]),
        "number" => (ValueKind::Number, false, &["placeholder"]),
        "select" => (
            ValueKind::Single,
            true,
            &["option", "optionQuery", "useLinks"],
        ),
        "inlineSelect" => (
            ValueKind::Single,
            false,
            &["option", "optionQuery", "useLinks"],
        ),
        "multiSelect" => (
            ValueKind::Multiple,
            true,
            &["option", "optionQuery", "useLinks"],
        ),
        "suggester" | "imageSuggester" => (
            ValueKind::Single,
            false,
            &[
                "option",
                "optionQuery",
                "optionSource",
                "useLinks",
                "allowOther",
            ],
        ),
        "date" | "datePicker" => (ValueKind::Date, false, &[]),
        "time" => (ValueKind::Time, false, &[]),
        "dateTime" => (ValueKind::DateTime, false, &[]),
        "editor" => (ValueKind::Text, true, &[]),
        "list" => (
            ValueKind::Multiple,
            true,
            &["placeholder", "limit", "multiLine"],
        ),
        "inlineList" => (ValueKind::Multiple, false, &["placeholder", "limit"]),
        "listSuggester" | "imageListSuggester" => (
            ValueKind::Multiple,
            true,
            &[
                "option",
                "optionQuery",
                "optionSource",
                "useLinks",
                "allowOther",
                "limit",
            ],
        ),
        "inlineListSuggester" => (
            ValueKind::Multiple,
            false,
            &[
                "option",
                "optionQuery",
                "optionSource",
                "useLinks",
                "allowOther",
                "limit",
            ],
        ),
        _ => return None,
    };
    Some(InputType {
        kind,
        block_only,
        arguments,
    })
}

const MULTI_ARGUMENTS: &[&str] = &["class", "option", "optionQuery", "optionSource"];

/// A diagnostic's level, as Meta Bind distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaBindLevel {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetaBindDiagnostic {
    pub level: MetaBindLevel,
    pub message: String,
}

impl MetaBindDiagnostic {
    fn error(message: impl Into<String>) -> Self {
        Self {
            level: MetaBindLevel::Error,
            message: message.into(),
        }
    }

    fn warning(message: impl Into<String>) -> Self {
        Self {
            level: MetaBindLevel::Warning,
            message: message.into(),
        }
    }

    fn info(message: impl Into<String>) -> Self {
        Self {
            level: MetaBindLevel::Info,
            message: message.into(),
        }
    }
}

/// An input's validated settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputSettings {
    pub input_type: String,
    pub options: Vec<MetaBindOption>,
    pub min: f64,
    pub max: f64,
    pub step: Option<f64>,
    pub on_value: Value,
    pub off_value: Value,
    pub limit: Option<usize>,
    pub default_value: Option<Value>,
    /// Suggesters accept values outside their options (`allowOther`).
    pub allow_other: bool,
    /// What a date or time input without a value shows: now, as Meta Bind
    /// shows it, when the render has a clock (see [`MetaBindEnv::now_ms`]).
    #[serde(skip)]
    pub now_default: Option<Value>,
}

/// A selectable value and its label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaBindOption {
    pub value: Value,
    pub label: String,
}

fn argument<'d>(declaration: &'d InputDeclaration, name: &str) -> Option<&'d MetaBindArgument> {
    declaration
        .arguments
        .iter()
        .rev()
        .find(|argument| argument.name == name)
}

fn number_argument(
    declaration: &InputDeclaration,
    name: &str,
    diagnostics: &mut Vec<MetaBindDiagnostic>,
) -> Option<f64> {
    let value = argument(declaration, name)?.values.first()?;
    if let Ok(number) = value.parse::<f64>() {
        Some(number)
    } else {
        diagnostics.push(MetaBindDiagnostic::warning(format!(
            "`{name}` expects a number, not `{value}`"
        )));
        None
    }
}

/// Checks an input declaration and works out its settings; errors among the
/// diagnostics mean the field cannot render.
#[must_use]
#[allow(clippy::too_many_lines)] // one check per Meta Bind rule
pub fn input_settings(
    declaration: &InputDeclaration,
    inline: bool,
    config: &MetaBindConfig,
) -> (Option<InputSettings>, Vec<MetaBindDiagnostic>) {
    let mut diagnostics = Vec::new();
    let Some(spec) = input_type(&declaration.input_type) else {
        diagnostics.push(MetaBindDiagnostic::error(format!(
            "unknown input field type `{}`",
            declaration.input_type
        )));
        return (None, diagnostics);
    };
    if inline && spec.block_only && !config.ignore_code_block_restrictions {
        diagnostics.push(MetaBindDiagnostic::error(format!(
            "`{}` inputs work only in a meta-bind code block",
            declaration.input_type
        )));
    }
    let mut seen = Vec::<&str>::new();
    for argument in &declaration.arguments {
        let name = argument.name.as_str();
        if !COMMON_ARGUMENTS.contains(&name) && !spec.arguments.contains(&name) {
            diagnostics.push(MetaBindDiagnostic::warning(format!(
                "`{name}` does not apply to `{}` inputs",
                declaration.input_type
            )));
        }
        if seen.contains(&name) && !MULTI_ARGUMENTS.contains(&name) {
            diagnostics.push(MetaBindDiagnostic::error(format!(
                "argument `{name}` is given more than once"
            )));
        }
        seen.push(name);
    }
    if let Some(target) = &declaration.target {
        if target.storage == MetaBindStorage::Scope {
            diagnostics.push(MetaBindDiagnostic::error(
                "`scope^` targets work only inside an API scope",
            ));
        }
    }

    let min = number_argument(declaration, "minValue", &mut diagnostics).unwrap_or(0.0);
    let max = number_argument(declaration, "maxValue", &mut diagnostics).unwrap_or(100.0);
    if spec.kind == ValueKind::Range && min >= max {
        diagnostics.push(MetaBindDiagnostic::error(format!(
            "minValue ({min}) must be less than maxValue ({max})"
        )));
    }
    let step = number_argument(declaration, "stepSize", &mut diagnostics);
    let limit = number_argument(declaration, "limit", &mut diagnostics)
        .filter(|limit| *limit >= 0.0)
        .map(|limit| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let limit = limit as usize;
            limit
        });
    let mut options = Vec::new();
    for argument in declaration
        .arguments
        .iter()
        .filter(|argument| argument.name == "option")
    {
        match argument.values.len() {
            1 | 2 => {
                let value = argument.literal(0).unwrap_or(Value::Null);
                let label = argument
                    .values
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| argument.values[0].clone());
                options.push(MetaBindOption { value, label });
            }
            count => diagnostics.push(MetaBindDiagnostic::warning(format!(
                "`option` takes a value and an optional label, not {count} values"
            ))),
        }
    }
    if argument(declaration, "optionSource").is_some() {
        diagnostics.push(MetaBindDiagnostic::info(
            "`optionSource` options come from Obsidian; Vulcan offers the `option` and `optionQuery` values",
        ));
    }
    let toggle_value = |name: &str, fallback: bool| {
        argument(declaration, name)
            .and_then(|argument| argument.literal(0))
            .unwrap_or(Value::Bool(fallback))
    };
    let settings = InputSettings {
        input_type: declaration.input_type.clone(),
        options,
        min,
        max,
        step,
        on_value: toggle_value("onValue", true),
        off_value: toggle_value("offValue", false),
        limit,
        default_value: argument(declaration, "defaultValue").and_then(|a| a.literal(0)),
        allow_other: argument(declaration, "allowOther")
            .is_some_and(|argument| argument.values.first().is_none_or(|v| v == "true")),
        now_default: None,
    };
    let failed = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.level == MetaBindLevel::Error);
    (if failed { None } else { Some(settings) }, diagnostics)
}

impl InputSettings {
    fn kind(&self) -> ValueKind {
        input_type(&self.input_type).map_or(ValueKind::Text, |spec| spec.kind)
    }

    /// The value the input shows for `stored`: the stored value when it has
    /// the input's type, otherwise the default.
    #[must_use]
    pub fn effective_value(&self, stored: Option<&Value>) -> Value {
        let fits = |value: &Value| match self.kind() {
            ValueKind::Toggle => true,
            ValueKind::Range | ValueKind::Number => value.is_number(),
            ValueKind::Text | ValueKind::Date | ValueKind::Time | ValueKind::DateTime => {
                value.is_string()
            }
            ValueKind::Single => !value.is_object() && !value.is_array(),
            ValueKind::Multiple => value.is_array(),
        };
        if let Some(value) = stored.filter(|value| !value.is_null() && fits(value)) {
            return value.clone();
        }
        if let Some(default) = &self.default_value {
            return default.clone();
        }
        if let Some(now) = &self.now_default {
            return now.clone();
        }
        match self.kind() {
            ValueKind::Toggle => self.off_value.clone(),
            ValueKind::Range => {
                serde_json::Number::from_f64(self.min).map_or(Value::Null, Value::Number)
            }
            ValueKind::Text => Value::String(String::new()),
            ValueKind::Multiple => Value::Array(Vec::new()),
            ValueKind::Number
            | ValueKind::Single
            | ValueKind::Date
            | ValueKind::Time
            | ValueKind::DateTime => Value::Null,
        }
    }

    /// The value shown as text: `[x]`/`[ ]` for toggles, `3 / 10` for
    /// sliders, the option label for selects, a comma list for lists.
    #[must_use]
    pub fn display(&self, value: &Value) -> String {
        match self.kind() {
            ValueKind::Toggle => {
                if *value == self.on_value {
                    "[x]".to_string()
                } else {
                    "[ ]".to_string()
                }
            }
            ValueKind::Range => format!("{} / {}", display_value(value), format_number(self.max)),
            ValueKind::Single => self
                .options
                .iter()
                .find(|option| option.value == *value)
                .map_or_else(|| display_value(value), |option| option.label.clone()),
            ValueKind::Multiple => match value {
                Value::Array(items) => items
                    .iter()
                    .map(|item| {
                        self.options
                            .iter()
                            .find(|option| option.value == *item)
                            .map_or_else(|| display_value(item), |option| option.label.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                other => display_value(other),
            },
            _ => display_value(value),
        }
    }

    /// Parses `text` into the value this input would store, checking it
    /// against the input's options, bounds, and limits.
    pub fn parse_value(&self, text: &str) -> Result<Value, String> {
        let kind = self.kind();
        match kind {
            ValueKind::Toggle => {
                let value = match text.trim() {
                    "on" | "toggle-on" => self.on_value.clone(),
                    "off" | "toggle-off" => self.off_value.clone(),
                    other => parse_literal(other),
                };
                if value == self.on_value || value == self.off_value {
                    Ok(value)
                } else {
                    Err(format!(
                        "a toggle stores {} or {}",
                        display_value(&self.on_value),
                        display_value(&self.off_value)
                    ))
                }
            }
            ValueKind::Range => {
                let number = text
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| format!("`{text}` is not a number"))?;
                if number < self.min || number > self.max {
                    return Err(format!(
                        "{} must lie between {} and {}",
                        format_number(number),
                        format_number(self.min),
                        format_number(self.max)
                    ));
                }
                number_value(number)
            }
            ValueKind::Number => {
                if text.trim().is_empty() || text.trim().eq_ignore_ascii_case("null") {
                    return Ok(Value::Null);
                }
                let number = text
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| format!("`{text}` is not a number"))?;
                number_value(number)
            }
            ValueKind::Text => {
                if let Some(limit) = self.limit {
                    if text.chars().count() > limit {
                        return Err(format!("the text is longer than the limit of {limit}"));
                    }
                }
                Ok(Value::String(text.to_string()))
            }
            ValueKind::Single => self.pick_option(text),
            ValueKind::Multiple => {
                let items = match serde_json::from_str::<Value>(text) {
                    Ok(Value::Array(items)) => items,
                    _ => text
                        .split(',')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(|item| self.pick_option(item))
                        .collect::<Result<Vec<_>, _>>()?,
                };
                if let Some(limit) = self.limit {
                    if items.len() > limit {
                        return Err(format!("the list is longer than the limit of {limit}"));
                    }
                }
                Ok(Value::Array(items))
            }
            ValueKind::Date | ValueKind::Time | ValueKind::DateTime => {
                let text = text.trim();
                let valid = match kind {
                    ValueKind::Date => chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok(),
                    ValueKind::Time => chrono::NaiveTime::parse_from_str(text, "%H:%M").is_ok(),
                    _ => chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M").is_ok(),
                };
                let format = match kind {
                    ValueKind::Date => "YYYY-MM-DD",
                    ValueKind::Time => "HH:mm",
                    _ => "YYYY-MM-DDTHH:mm",
                };
                if valid || (self.input_type == "datePicker" && !text.is_empty()) {
                    Ok(Value::String(text.to_string()))
                } else {
                    Err(format!("`{text}` is not a {format} value"))
                }
            }
        }
    }

    fn pick_option(&self, text: &str) -> Result<Value, String> {
        let wanted = parse_literal(text.trim());
        if self.options.is_empty() || self.allow_other {
            return Ok(wanted);
        }
        self.options
            .iter()
            .find(|option| option.value == wanted || option.label == text.trim())
            .map(|option| option.value.clone())
            .ok_or_else(|| {
                format!(
                    "`{text}` is not one of: {}",
                    self.options
                        .iter()
                        .map(|option| display_value(&option.value))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// The values a picker offers: toggle states or the declared options.
    #[must_use]
    pub fn choices(&self) -> Vec<MetaBindOption> {
        if self.kind() == ValueKind::Toggle {
            return vec![
                MetaBindOption {
                    value: self.on_value.clone(),
                    label: "on".to_string(),
                },
                MetaBindOption {
                    value: self.off_value.clone(),
                    label: "off".to_string(),
                },
            ];
        }
        if matches!(self.kind(), ValueKind::Single) {
            return self.options.clone();
        }
        Vec::new()
    }
}

fn number_value(number: f64) -> Result<Value, String> {
    if number.fract() == 0.0 && number.abs() < 9e15 {
        #[allow(clippy::cast_possible_truncation)]
        return Ok(Value::from(number as i64));
    }
    serde_json::Number::from_f64(number)
        .map(Value::Number)
        .ok_or_else(|| "not a finite number".to_string())
}

fn format_number(number: f64) -> String {
    number_value(number).map_or_else(|_| number.to_string(), |value| value.to_string())
}

/// A value as text: strings bare, `null` as an empty string.
fn display_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(display_value)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

// ------------------------------------------------------------- reading

/// Reads `path` inside `root`.
#[must_use]
pub fn read_path<'v>(root: &'v Value, path: &[PathSegment]) -> Option<&'v Value> {
    path.iter().try_fold(root, |value, segment| match segment {
        PathSegment::Key(key) => value.get(key),
        PathSegment::Index(index) => value.get(index),
    })
}

/// Writes `value` at `path` inside `root`, creating missing objects and
/// lists on the way.
pub fn write_path(root: &mut Value, path: &[PathSegment], value: Value) -> Result<(), String> {
    let Some((last, parents)) = path.split_last() else {
        *root = value;
        return Ok(());
    };
    let mut current = root;
    for (index, segment) in parents.iter().enumerate() {
        let next_is_index = matches!(path[index + 1], PathSegment::Index(_));
        current = step_mut(current, segment, next_is_index)?;
    }
    match last {
        PathSegment::Key(key) => {
            if !current.is_object() {
                if !current.is_null() {
                    return Err(format!("`{key}` sits inside a value that is not an object"));
                }
                *current = Value::Object(Map::new());
            }
            current
                .as_object_mut()
                .expect("object")
                .insert(key.clone(), value);
        }
        PathSegment::Index(position) => {
            let items = list_mut(current)?;
            if items.len() <= *position {
                items.resize(position + 1, Value::Null);
            }
            items[*position] = value;
        }
    }
    Ok(())
}

fn step_mut<'v>(
    current: &'v mut Value,
    segment: &PathSegment,
    next_is_index: bool,
) -> Result<&'v mut Value, String> {
    let empty = || {
        if next_is_index {
            Value::Array(Vec::new())
        } else {
            Value::Object(Map::new())
        }
    };
    match segment {
        PathSegment::Key(key) => {
            if current.is_null() {
                *current = Value::Object(Map::new());
            }
            let object = current
                .as_object_mut()
                .ok_or_else(|| format!("`{key}` sits inside a value that is not an object"))?;
            let entry = object.entry(key.clone()).or_insert_with(empty);
            if entry.is_null() {
                *entry = empty();
            }
            Ok(entry)
        }
        PathSegment::Index(position) => {
            let list = list_mut(current)?;
            if list.len() <= *position {
                list.resize(position + 1, Value::Null);
            }
            if list[*position].is_null() {
                list[*position] = empty();
            }
            Ok(&mut list[*position])
        }
    }
}

fn list_mut(value: &mut Value) -> Result<&mut Vec<Value>, String> {
    if value.is_null() {
        *value = Value::Array(Vec::new());
    }
    value
        .as_array_mut()
        .ok_or_else(|| "an index into a value that is not a list".to_string())
}

/// The note's frontmatter as JSON.
#[must_use]
pub fn frontmatter_json(parsed: &ParsedDocument) -> Value {
    parsed
        .frontmatter
        .as_ref()
        .and_then(|frontmatter| serde_json::to_value(frontmatter).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()))
}

/// Resolves the note a target names from `source_path`, with Obsidian link
/// resolution.
#[must_use]
pub fn resolve_target_note<'i, S: std::hash::BuildHasher>(
    index: &'i HashMap<String, NoteRecord, S>,
    source_path: &str,
    file: &str,
) -> Option<&'i NoteRecord> {
    let path = crate::note_lookup::resolve_identity(
        || {
            index.values().map(|note| crate::note_lookup::NoteIdentity {
                path: &note.document_path,
                file_name: &note.file_name,
                aliases: &note.aliases,
            })
        },
        |key| index.get(key).map(|note| note.document_path.as_str()),
        source_path,
        file,
    )?;
    index.values().find(|note| note.document_path == path)
}

/// The values a note's fields read and write while it renders.
struct Values<'a, S> {
    frontmatter: Value,
    memory: Value,
    note_index: Option<&'a HashMap<String, NoteRecord, S>>,
    source_path: Option<&'a str>,
}

impl<S: std::hash::BuildHasher> Values<'_, S> {
    fn read(&self, target: &BindTarget) -> Result<Option<Value>, String> {
        let root_value;
        let root = match (target.storage, &target.file) {
            (MetaBindStorage::Memory | MetaBindStorage::GlobalMemory, _) => &self.memory,
            (MetaBindStorage::Scope, _) => {
                return Err("`scope^` targets work only inside an API scope".to_string())
            }
            (MetaBindStorage::Frontmatter, None) => &self.frontmatter,
            (MetaBindStorage::Frontmatter, Some(file)) => {
                let index = self
                    .note_index
                    .ok_or_else(|| format!("cannot read `{file}`: the vault is not indexed"))?;
                let note = resolve_target_note(index, self.source_path.unwrap_or_default(), file)
                    .ok_or_else(|| format!("no note `{file}`"))?;
                root_value = note.properties.clone();
                &root_value
            }
        };
        let root = if matches!(
            target.storage,
            MetaBindStorage::Memory | MetaBindStorage::GlobalMemory
        ) {
            let scope = if target.storage == MetaBindStorage::Memory {
                "memory"
            } else {
                "global"
            };
            match root.get(scope) {
                Some(value) => value,
                None => return Ok(None),
            }
        } else {
            root
        };
        Ok(read_path(root, &target.path).cloned())
    }

    fn write(&mut self, target: &BindTarget, value: Value) {
        match (target.storage, &target.file) {
            (MetaBindStorage::Memory, _) => {
                let _ = write_path(&mut self.memory, &memory_path("memory", target), value);
            }
            (MetaBindStorage::GlobalMemory, _) => {
                let _ = write_path(&mut self.memory, &memory_path("global", target), value);
            }
            (MetaBindStorage::Frontmatter, None) => {
                let _ = write_path(&mut self.frontmatter, &target.path, value);
            }
            _ => {}
        }
    }
}

fn memory_path(scope: &str, target: &BindTarget) -> Vec<PathSegment> {
    std::iter::once(PathSegment::Key(scope.to_string()))
        .chain(target.path.iter().cloned())
        .collect()
}

// ------------------------------------------------------------- rendering

/// What a field renders as.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetaBindOutcome {
    Input {
        settings: Box<InputSettings>,
        target: Option<String>,
        value: Value,
        display: String,
    },
    View {
        view_type: String,
        /// Markdown for `text` views with `renderMarkdown`, links, and images;
        /// plain text otherwise.
        text: String,
        markdown: bool,
        /// The value written back to `write_target`.
        value: Value,
        write_target: Option<String>,
    },
    Buttons {
        labels: Vec<String>,
    },
    Embed {
        target: String,
    },
    Hidden,
    Notice {
        message: String,
    },
    Error {
        message: String,
    },
}

/// One rendered field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaBindField {
    pub byte_range: Range<usize>,
    /// One-based line of the field.
    pub line_number: usize,
    /// The declaration as written.
    pub declaration: String,
    pub block: bool,
    #[serde(skip)]
    pub parsed: Option<MetaBindDeclaration>,
    pub outcome: MetaBindOutcome,
    pub diagnostics: Vec<MetaBindDiagnostic>,
}

/// Whether `path` lies under one of the configured excluded folders.
#[must_use]
pub fn is_excluded(config: &MetaBindConfig, path: &str) -> bool {
    config.excluded_folders.iter().any(|folder| {
        let folder = folder.trim_matches('/');
        !folder.is_empty() && (path == folder || path.starts_with(&format!("{folder}/")))
    })
}

/// What a render may use beyond the note and the index.
#[derive(Default)]
pub struct MetaBindEnv<'a> {
    /// Now, in milliseconds since the Unix epoch: date and time inputs
    /// without a value show it, as Meta Bind's show today. `None` for
    /// reproducible output, where they show empty.
    pub now_ms: Option<i64>,
    /// The vault, for the image files `imageSuggester(optionQuery("folder"))`
    /// offers.
    pub paths: Option<&'a VaultPaths>,
    /// Runs `meta-bind-js-view` blocks when `metabind.enable_js` is on;
    /// without it they show a notice.
    pub scripts: Option<&'a dyn MetaBindScripts>,
}

impl MetaBindEnv<'_> {
    /// An environment with the device clock and no vault access.
    #[must_use]
    pub fn with_clock() -> Self {
        Self {
            now_ms: Some(crate::current_utc_timestamp_ms()),
            ..Self::default()
        }
    }
}

/// Renders the note's fields in document order: inputs show their current
/// values, and views compute from them, seeing earlier views' write-backs.
#[must_use]
pub fn evaluate_note_fields<S: std::hash::BuildHasher>(
    config: &VaultConfig,
    note_index: Option<&HashMap<String, NoteRecord, S>>,
    source_path: Option<&str>,
    parsed: &ParsedDocument,
    env: &MetaBindEnv<'_>,
) -> Vec<MetaBindField> {
    let settings = &config.metabind;
    if !settings.enabled || parsed.meta_bind_fields.is_empty() {
        return Vec::new();
    }
    let excluded = source_path.is_some_and(|path| is_excluded(settings, path));
    let mut values = Values {
        frontmatter: frontmatter_json(parsed),
        memory: Value::Object(Map::new()),
        note_index,
        source_path,
    };
    let mut math = None;
    let buttons = button_labels(parsed, settings);
    let mut fields = Vec::new();
    for raw in &parsed.meta_bind_fields {
        let block = raw.kind != MetaBindSourceKind::Inline;
        let mut field = MetaBindField {
            byte_range: raw.byte_range.clone(),
            line_number: raw.line_number,
            declaration: raw.text.trim().to_string(),
            block,
            parsed: None,
            outcome: MetaBindOutcome::Hidden,
            diagnostics: Vec::new(),
        };
        if excluded {
            field.outcome = MetaBindOutcome::Notice {
                message: "Meta Bind fields do not render in excluded folders".to_string(),
            };
            fields.push(field);
            continue;
        }
        let declaration = match raw.kind {
            MetaBindSourceKind::Inline | MetaBindSourceKind::Block => {
                parse_declaration(&raw.text, settings)
            }
            MetaBindSourceKind::ButtonBlock => yaml_serde::from_str::<yaml_serde::Value>(&raw.text)
                .map_err(|error| format!("invalid button YAML: {error}"))
                .and_then(|yaml| serde_json::to_value(yaml).map_err(|error| error.to_string()))
                .map(|button| MetaBindDeclaration::Button { button }),
            MetaBindSourceKind::JsView => parse_js_view(&raw.text).map(MetaBindDeclaration::JsView),
            MetaBindSourceKind::Embed => Ok(MetaBindDeclaration::Embed {
                target: raw.text.trim().to_string(),
            }),
        };
        match declaration {
            Err(message) => {
                field.diagnostics.push(MetaBindDiagnostic::error(&message));
                field.outcome = MetaBindOutcome::Error { message };
            }
            Ok(declaration) => {
                field.outcome = render_declaration(
                    &declaration,
                    !block,
                    settings,
                    config,
                    env,
                    &mut values,
                    &mut math,
                    &buttons,
                    &mut field.diagnostics,
                );
                field.parsed = Some(declaration);
            }
        }
        fields.push(field);
    }
    fields
}

/// Labels of the buttons a note can reference: its `meta-bind-button`
/// blocks and the configured button templates.
fn button_labels(parsed: &ParsedDocument, config: &MetaBindConfig) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    let mut add = |button: &Value| {
        if let (Some(id), Some(label)) = (
            button.get("id").and_then(Value::as_str),
            button.get("label").and_then(Value::as_str),
        ) {
            labels.insert(id.to_string(), label.to_string());
        }
    };
    for button in &config.button_templates {
        add(button);
    }
    for raw in &parsed.meta_bind_fields {
        if raw.kind == MetaBindSourceKind::ButtonBlock {
            if let Some(button) = yaml_serde::from_str::<yaml_serde::Value>(&raw.text)
                .ok()
                .and_then(|yaml| serde_json::to_value(yaml).ok())
            {
                add(&button);
            }
        }
    }
    labels
}

#[allow(clippy::too_many_arguments)]
fn render_declaration<S: std::hash::BuildHasher>(
    declaration: &MetaBindDeclaration,
    inline: bool,
    settings: &MetaBindConfig,
    config: &VaultConfig,
    env: &MetaBindEnv<'_>,
    values: &mut Values<'_, S>,
    math: &mut Option<crate::numerals::MathEvaluator>,
    buttons: &HashMap<String, String>,
    diagnostics: &mut Vec<MetaBindDiagnostic>,
) -> MetaBindOutcome {
    match declaration {
        MetaBindDeclaration::Input(input) => {
            render_input(input, inline, settings, env, values, diagnostics)
        }
        MetaBindDeclaration::View(view) => {
            match render_view(view, settings, config, values, math, diagnostics) {
                Ok(outcome) => outcome,
                Err(message) => {
                    diagnostics.push(MetaBindDiagnostic::error(&message));
                    MetaBindOutcome::Error { message }
                }
            }
        }
        MetaBindDeclaration::Buttons { ids } => {
            let mut labels = Vec::new();
            for id in ids {
                if let Some(label) = buttons.get(id) {
                    labels.push(label.clone());
                } else {
                    diagnostics.push(MetaBindDiagnostic::error(format!(
                        "no button with id `{id}`"
                    )));
                }
            }
            if labels.is_empty() {
                return first_error(diagnostics);
            }
            MetaBindOutcome::Buttons { labels }
        }
        MetaBindDeclaration::Button { button } => {
            if button.get("hidden").and_then(Value::as_bool) == Some(true) {
                return MetaBindOutcome::Hidden;
            }
            let Some(label) = button.get("label").and_then(Value::as_str) else {
                let message = "a button needs a `label`".to_string();
                diagnostics.push(MetaBindDiagnostic::error(&message));
                return MetaBindOutcome::Error { message };
            };
            MetaBindOutcome::Buttons {
                labels: vec![label.to_string()],
            }
        }
        MetaBindDeclaration::JsView(view) => {
            match render_js_view(view, settings, env, values, diagnostics) {
                Ok(outcome) => outcome,
                Err(message) => {
                    diagnostics.push(MetaBindDiagnostic::error(&message));
                    MetaBindOutcome::Error { message }
                }
            }
        }
        MetaBindDeclaration::Embed { target } => {
            let inner = target
                .trim()
                .trim_start_matches('!')
                .trim_start_matches("[[")
                .trim_end_matches("]]")
                .to_string();
            if inner.is_empty() {
                let message = "meta-bind-embed names no note".to_string();
                diagnostics.push(MetaBindDiagnostic::error(&message));
                return MetaBindOutcome::Error { message };
            }
            MetaBindOutcome::Embed { target: inner }
        }
    }
}

/// Runs a JS view with its bound values; its result is saved to `save to`
/// for the views after it, as view write-backs are.
fn render_js_view<S: std::hash::BuildHasher>(
    view: &JsViewDeclaration,
    settings: &MetaBindConfig,
    env: &MetaBindEnv<'_>,
    values: &mut Values<'_, S>,
    diagnostics: &mut Vec<MetaBindDiagnostic>,
) -> Result<MetaBindOutcome, String> {
    if !settings.enable_js {
        return Ok(MetaBindOutcome::Notice {
            message: "JS views run when `metabind.enable_js` is on".to_string(),
        });
    }
    let Some(scripts) = env.scripts else {
        return Ok(MetaBindOutcome::Notice {
            message: "JS views run only where DataviewJS does".to_string(),
        });
    };
    let mut bound = Map::new();
    for (name, target) in &view.bindings {
        if matches!(
            target.storage,
            MetaBindStorage::Memory | MetaBindStorage::GlobalMemory
        ) {
            diagnostics.push(MetaBindDiagnostic::info(
                "memory values last only while Obsidian has the note open",
            ));
        }
        bound.insert(name.clone(), values.read(target)?.unwrap_or(Value::Null));
    }
    let path = values.source_path.unwrap_or_default();
    let context = serde_json::json!({
        "bound": bound,
        "metadata": { "frontmatter": values.frontmatter },
        "file": {
            "path": path,
            "name": path.rsplit('/').next().unwrap_or(path).trim_end_matches(".md"),
        },
    });
    let Some(result) = scripts.run_js_view(&view.code, &context) else {
        return Ok(MetaBindOutcome::Notice {
            message: "DataviewJS is off for this render, so JS views do not run".to_string(),
        });
    };
    let (text, markdown, value) = match result? {
        MetaBindScriptValue::Markdown(text) => (text.clone(), true, Value::String(text)),
        MetaBindScriptValue::Value(value) => {
            let text = match &value {
                Value::Null => String::new(),
                Value::String(text) => text.clone(),
                other => display_value(other),
            };
            (text, false, value)
        }
    };
    if let Some(target) = &view.save_to {
        values.write(target, value.clone());
    }
    Ok(MetaBindOutcome::View {
        view_type: "js".to_string(),
        text: if view.hidden { String::new() } else { text },
        markdown,
        value,
        write_target: view.save_to.as_ref().map(ToString::to_string),
    })
}

/// Now as a date or time input shows it, in the device's time zone.
fn now_value(input_type: &str, now_ms: i64, preferred_date_format: &str) -> Option<Value> {
    let now = jiff::Timestamp::from_millisecond(now_ms)
        .ok()?
        .to_zoned(jiff::tz::TimeZone::system());
    let text = match input_type {
        "date" => now.strftime("%Y-%m-%d").to_string(),
        "time" => now.strftime("%H:%M").to_string(),
        "dateTime" => now.strftime("%Y-%m-%dT%H:%M").to_string(),
        "datePicker" => moment_date(now.date(), preferred_date_format),
        _ => return None,
    };
    Some(Value::String(text))
}

/// `date` in a Moment.js format's year, month, and day tokens (`YYYY`,
/// `YY`, `MM`, `M`, `DD`, `D`); `[...]` is literal text, as in Moment.
fn moment_date(date: jiff::civil::Date, format: &str) -> String {
    let mut out = String::new();
    let mut rest = if format.is_empty() {
        "YYYY-MM-DD"
    } else {
        format
    };
    while let Some(character) = rest.chars().next() {
        if character == '[' {
            if let Some(end) = rest.find(']') {
                out.push_str(&rest[1..end]);
                rest = &rest[end + 1..];
                continue;
            }
        }
        let token = ["YYYY", "YY", "MM", "M", "DD", "D"]
            .into_iter()
            .find(|token| rest.starts_with(token));
        match token {
            Some("YYYY") => write!(out, "{:04}", date.year()),
            Some("YY") => write!(out, "{:02}", i32::from(date.year()).rem_euclid(100)),
            Some("MM") => write!(out, "{:02}", date.month()),
            Some("M") => write!(out, "{}", date.month()),
            Some("DD") => write!(out, "{:02}", date.day()),
            Some("D") => write!(out, "{}", date.day()),
            _ => {
                out.push(character);
                rest = &rest[character.len_utf8()..];
                continue;
            }
        }
        .expect("writing to a String cannot fail");
        rest = &rest[token.map_or(0, str::len)..];
    }
    out
}

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "avif"];

/// Adds the options of the input's `optionQuery` arguments: the notes a
/// Dataview source selects (as links, per `useLinks`), or for image
/// suggesters the images in a folder.
fn add_query_options<S: std::hash::BuildHasher>(
    settings: &mut InputSettings,
    declaration: &InputDeclaration,
    values: &Values<'_, S>,
    paths: Option<&VaultPaths>,
    diagnostics: &mut Vec<MetaBindDiagnostic>,
) {
    let queries = declaration
        .arguments
        .iter()
        .filter(|argument| argument.name == "optionQuery")
        .map(|argument| argument.values.first().map_or("", String::as_str))
        .collect::<Vec<_>>();
    if queries.is_empty() {
        return;
    }
    let use_links = argument(declaration, "useLinks").map_or("true", |argument| {
        argument.values.first().map_or("true", String::as_str)
    });
    for query in queries {
        if settings.input_type.starts_with("image") {
            let Some(paths) = paths else {
                diagnostics.push(MetaBindDiagnostic::info(
                    "image options need the vault's files, which this view does not read",
                ));
                // Without them, accept any value rather than refuse real options.
                settings.allow_other = true;
                continue;
            };
            for image in image_files(paths, unquote(query)) {
                let label = image.rsplit('/').next().unwrap_or(&image).to_string();
                settings.options.push(MetaBindOption {
                    value: Value::String(image),
                    label,
                });
            }
            continue;
        }
        let Some(index) = values.note_index else {
            diagnostics.push(MetaBindDiagnostic::info(
                "`optionQuery` options need the vault index; run `vulcan scan`",
            ));
            // Without them, accept any value rather than refuse real options.
            settings.allow_other = true;
            continue;
        };
        match query_notes(query, index, values.source_path) {
            Ok(notes) => {
                for note in notes {
                    let name = note.file_name.clone();
                    let value = match use_links {
                        "false" => name.clone(),
                        "partial" => format!("[[{name}]]"),
                        _ => format!("[[{}|{name}]]", note.document_path),
                    };
                    settings.options.push(MetaBindOption {
                        value: Value::String(value),
                        label: name,
                    });
                }
            }
            Err(message) => diagnostics.push(MetaBindDiagnostic::warning(format!(
                "`optionQuery({query})`: {message}"
            ))),
        }
    }
}

fn unquote(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(text)
}

/// The image files under `folder` (the whole vault for `""`), sorted.
fn image_files(paths: &VaultPaths, folder: &str) -> Vec<String> {
    let folder = folder.trim_matches('/');
    let root = paths.vault_root();
    let mut images = ignore::WalkBuilder::new(root.join(folder))
        .hidden(true)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    IMAGE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
                })
        })
        .filter_map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .ok()
                .map(|path| path.to_string_lossy().replace('\\', "/"))
        })
        .collect::<Vec<_>>();
    images.sort();
    images
}

/// The notes a Dataview source (`#tag`, `"folder"`, `[[note]]`,
/// `outgoing([[note]])`, combined with `and`, `or`, and `-`) selects, by
/// path.
fn query_notes<'i, S: std::hash::BuildHasher>(
    source: &str,
    index: &'i HashMap<String, NoteRecord, S>,
    source_path: Option<&str>,
) -> Result<Vec<&'i NoteRecord>, String> {
    let query = crate::dql::parse_dql(&format!("LIST FROM {source}"))?;
    let Some(expr) = query.commands.iter().find_map(|command| match command {
        crate::dql::DqlDataCommand::From(expr) => Some(expr),
        _ => None,
    }) else {
        return Err("not a Dataview source".to_string());
    };
    let mut notes = Vec::new();
    for note in index.values() {
        if source_selects(expr, note, index, source_path)? {
            notes.push(note);
        }
    }
    notes.sort_by(|left, right| left.document_path.cmp(&right.document_path));
    Ok(notes)
}

fn link_key(path: &str) -> String {
    format!("[[{}]]", path.strip_suffix(".md").unwrap_or(path))
}

fn normalize_tag(tag: &str) -> String {
    tag.trim_start_matches('#').to_lowercase()
}

fn source_selects<S: std::hash::BuildHasher>(
    expr: &crate::dql::DqlSourceExpr,
    note: &NoteRecord,
    index: &HashMap<String, NoteRecord, S>,
    source_path: Option<&str>,
) -> Result<bool, String> {
    use crate::dql::{DqlLinkTarget, DqlSourceExpr};
    let target = |target: &DqlLinkTarget| -> Result<&NoteRecord, String> {
        match target {
            DqlLinkTarget::SelfReference => source_path
                .and_then(|path| index.values().find(|note| note.document_path == path))
                .ok_or_else(|| "`[[]]` needs the current note".to_string()),
            DqlLinkTarget::Wikilink(raw) => {
                let inner = raw.trim().trim_start_matches("[[").trim_end_matches("]]");
                let inner = inner.split(['|', '#']).next().unwrap_or_default();
                resolve_target_note(index, source_path.unwrap_or_default(), inner)
                    .ok_or_else(|| format!("no note `{inner}`"))
            }
        }
    };
    Ok(match expr {
        DqlSourceExpr::Tag(tag) => {
            let wanted = normalize_tag(tag);
            note.tags.iter().any(|tag| {
                let tag = normalize_tag(tag);
                tag == wanted || tag.starts_with(&format!("{wanted}/"))
            })
        }
        DqlSourceExpr::Path(path) => {
            let path = path.trim_matches('/');
            path.is_empty()
                || note.document_path == path
                || note.document_path == format!("{path}.md")
                || note.document_path.starts_with(&format!("{path}/"))
        }
        DqlSourceExpr::IncomingLink(linked) => target(linked)?
            .inlinks
            .contains(&link_key(&note.document_path)),
        DqlSourceExpr::OutgoingLink(linking) => target(linking)?
            .links
            .contains(&link_key(&note.document_path)),
        DqlSourceExpr::Not(inner) => !source_selects(inner, note, index, source_path)?,
        DqlSourceExpr::And(left, right) => {
            source_selects(left, note, index, source_path)?
                && source_selects(right, note, index, source_path)?
        }
        DqlSourceExpr::Or(left, right) => {
            source_selects(left, note, index, source_path)?
                || source_selects(right, note, index, source_path)?
        }
    })
}

fn render_input<S: std::hash::BuildHasher>(
    input: &InputDeclaration,
    inline: bool,
    settings: &MetaBindConfig,
    env: &MetaBindEnv<'_>,
    values: &Values<'_, S>,
    diagnostics: &mut Vec<MetaBindDiagnostic>,
) -> MetaBindOutcome {
    let (input_settings, mut found) = input_settings(input, inline, settings);
    diagnostics.append(&mut found);
    let Some(mut input_settings) = input_settings else {
        return first_error(diagnostics);
    };
    if let Some(now) = env.now_ms {
        input_settings.now_default = now_value(
            &input_settings.input_type,
            now,
            &settings.preferred_date_format,
        );
    }
    add_query_options(&mut input_settings, input, values, env.paths, diagnostics);
    let stored = match &input.target {
        Some(target) => {
            if matches!(
                target.storage,
                MetaBindStorage::Memory | MetaBindStorage::GlobalMemory
            ) {
                diagnostics.push(MetaBindDiagnostic::info(
                    "memory values last only while Obsidian has the note open",
                ));
            }
            match values.read(target) {
                Ok(value) => value,
                Err(message) => {
                    diagnostics.push(MetaBindDiagnostic::error(&message));
                    return MetaBindOutcome::Error { message };
                }
            }
        }
        None => None,
    };
    let value = input_settings.effective_value(stored.as_ref());
    MetaBindOutcome::Input {
        display: input_settings.display(&value),
        target: input.target.as_ref().map(ToString::to_string),
        settings: Box::new(input_settings),
        value,
    }
}

fn first_error(diagnostics: &[MetaBindDiagnostic]) -> MetaBindOutcome {
    MetaBindOutcome::Error {
        message: diagnostics
            .iter()
            .find(|diagnostic| diagnostic.level == MetaBindLevel::Error)
            .map_or_else(|| "invalid field".to_string(), |d| d.message.clone()),
    }
}

fn render_view<S: std::hash::BuildHasher>(
    view: &ViewDeclaration,
    settings: &MetaBindConfig,
    config: &VaultConfig,
    values: &mut Values<'_, S>,
    math: &mut Option<crate::numerals::MathEvaluator>,
    diagnostics: &mut Vec<MetaBindDiagnostic>,
) -> Result<MetaBindOutcome, String> {
    let mut read = Vec::new();
    for part in &view.parts {
        if let ViewPart::Target { target } = part {
            read.push(values.read(target)?);
        }
    }
    let render_markdown = view
        .arguments
        .iter()
        .any(|argument| argument.name == "renderMarkdown");
    for argument in &view.arguments {
        let known: &[&str] = match view.view_type.as_str() {
            "text" => &["class", "renderMarkdown", "hidden"],
            _ => &["class", "hidden"],
        };
        if !known.contains(&argument.name.as_str()) {
            diagnostics.push(MetaBindDiagnostic::warning(format!(
                "`{}` does not apply to `{}` views",
                argument.name, view.view_type
            )));
        }
    }
    let (text, value, markdown) = match view.view_type.as_str() {
        "math" => {
            let mut expression = String::new();
            let mut scope = Map::new();
            let mut index = 0;
            for part in &view.parts {
                match part {
                    ViewPart::Text { text } => expression.push_str(text),
                    ViewPart::Target { .. } => {
                        let name = format!("__mb{index}");
                        let value = read[index]
                            .clone()
                            .filter(|value| !value.is_null())
                            .unwrap_or_else(|| Value::String(String::new()));
                        scope.insert(name.clone(), value);
                        expression.push_str(&name);
                        index += 1;
                    }
                }
            }
            let evaluator =
                math.get_or_insert_with(|| crate::numerals::MathEvaluator::start(config));
            let (text, value) = evaluator.evaluate(&expression, &Value::Object(scope))?;
            (text, value, false)
        }
        "text" => {
            let mut text = String::new();
            let mut index = 0;
            for part in &view.parts {
                match part {
                    ViewPart::Text { text: piece } => text.push_str(piece),
                    ViewPart::Target { .. } => {
                        text.push_str(&text_view_value(
                            read[index].as_ref(),
                            settings.view_field_display_null_as_empty,
                        ));
                        index += 1;
                    }
                }
            }
            (text.clone(), Value::String(text), render_markdown)
        }
        "link" | "image" => {
            let value = read.first().cloned().flatten().unwrap_or(Value::Null);
            let items = match &value {
                Value::Array(items) => items.clone(),
                Value::Null => Vec::new(),
                other => vec![other.clone()],
            };
            let image = view.view_type == "image";
            let rendered = items
                .iter()
                .map(|item| link_markdown(&display_value(item), image))
                .collect::<Vec<_>>()
                .join(if image { "\n" } else { ", " });
            (rendered, value, true)
        }
        "hidden" => (String::new(), Value::Null, false),
        other => return Err(format!("unknown view field type `{other}`")),
    };
    if let Some(target) = &view.write_target {
        values.write(target, value.clone());
    }
    if view.view_type == "hidden"
        || view
            .arguments
            .iter()
            .any(|argument| argument.name == "hidden")
    {
        return Ok(MetaBindOutcome::Hidden);
    }
    Ok(MetaBindOutcome::View {
        view_type: view.view_type.clone(),
        text,
        markdown,
        value,
        write_target: view.write_target.as_ref().map(ToString::to_string),
    })
}

fn text_view_value(value: Option<&Value>, null_as_empty: bool) -> String {
    match value {
        None | Some(Value::Null) => {
            if null_as_empty {
                String::new()
            } else {
                "null".to_string()
            }
        }
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| text_view_value(Some(item), null_as_empty))
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => other.to_string(),
    }
}

fn link_markdown(value: &str, image: bool) -> String {
    let value = value.trim();
    let bare = value
        .trim_start_matches('!')
        .trim_start_matches("[[")
        .trim_end_matches("]]");
    if value.starts_with("http://") || value.starts_with("https://") {
        if image {
            format!("![]({value})")
        } else {
            format!("[{value}]({value})")
        }
    } else if image {
        format!("![[{bare}]]")
    } else {
        format!("[[{bare}]]")
    }
}

/// The field as Markdown: `(block, markdown)`.
#[must_use]
pub fn field_markdown(field: &MetaBindField) -> (bool, String) {
    let text = match &field.outcome {
        MetaBindOutcome::Input { display, .. } => display.clone(),
        MetaBindOutcome::View { text, markdown, .. } => {
            if *markdown {
                text.clone()
            } else {
                escape_markdown(text)
            }
        }
        MetaBindOutcome::Buttons { labels } => labels
            .iter()
            .map(|label| format!("[{}]", escape_markdown(label)))
            .collect::<Vec<_>>()
            .join(" "),
        MetaBindOutcome::Embed { target } => format!("![[{target}]]"),
        MetaBindOutcome::Hidden => String::new(),
        MetaBindOutcome::Notice { message } => {
            if field.block {
                return (true, format!("> [!info] Meta Bind\n> {message}"));
            }
            format!("*({message})*")
        }
        MetaBindOutcome::Error { message } => {
            if field.block {
                return (true, format!("> [!warning] Meta Bind\n> {message}"));
            }
            format!("*(Meta Bind: {message})*")
        }
    };
    (field.block, text)
}

fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '*' | '_' | '[' | ']' | '`' | '\\' | '<') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// The field as HTML, with disabled form controls for inputs.
#[must_use]
pub fn field_html(field: &MetaBindField, escape: impl Fn(&str) -> String) -> Option<String> {
    let tag = if field.block { "div" } else { "span" };
    let html = match &field.outcome {
        MetaBindOutcome::Input {
            settings,
            value,
            display,
            ..
        } => {
            let control = match settings.kind() {
                ValueKind::Toggle => format!(
                    "<input type=\"checkbox\" disabled{}>",
                    if *value == settings.on_value {
                        " checked"
                    } else {
                        ""
                    }
                ),
                ValueKind::Range if settings.input_type == "progressBar" => format!(
                    "<progress max=\"{}\" value=\"{}\"></progress>",
                    format_number(settings.max - settings.min),
                    value.as_f64().map_or(0.0, |v| v - settings.min)
                ),
                ValueKind::Range => format!(
                    "<input type=\"range\" min=\"{}\" max=\"{}\" value=\"{}\" disabled>",
                    format_number(settings.min),
                    format_number(settings.max),
                    escape(&display_value(value))
                ),
                ValueKind::Single if !settings.options.is_empty() => {
                    let mut select = "<select disabled>".to_string();
                    for option in &settings.options {
                        let _ = write!(
                            select,
                            "<option{}>{}</option>",
                            if option.value == *value {
                                " selected"
                            } else {
                                ""
                            },
                            escape(&option.label)
                        );
                    }
                    select.push_str("</select>");
                    select
                }
                _ => format!(
                    "<input type=\"text\" value=\"{}\" disabled>",
                    escape(display).replace('"', "&quot;")
                ),
            };
            format!(
                "<{tag} class=\"mb-input mb-input-{}\">{control}</{tag}>",
                settings.input_type
            )
        }
        _ => return None,
    };
    Some(html)
}

// --------------------------------------------------------------- writing

/// The inputs in `fields` bound to `target` (as written, compared after
/// parsing) in the note itself.
#[must_use]
pub fn inputs_bound_to<'f>(
    fields: &'f [MetaBindField],
    target: &BindTarget,
) -> Vec<&'f InputSettings> {
    fields
        .iter()
        .filter_map(|field| match (&field.parsed, &field.outcome) {
            (
                Some(MetaBindDeclaration::Input(InputDeclaration {
                    target: Some(bound),
                    ..
                })),
                MetaBindOutcome::Input { settings, .. },
            ) if bound == target => Some(settings.as_ref()),
            _ => None,
        })
        .collect()
}

/// `source` with `value` written at `target` (a frontmatter target with no
/// file, or one whose file the caller has already read into `source`),
/// keeping the rest of the frontmatter as written; `None` when nothing
/// changes.
pub fn write_target_in_source(
    source: &str,
    path: &str,
    target: &BindTarget,
    value: &Value,
) -> Result<Option<String>, String> {
    let Some(PathSegment::Key(root)) = target.path.first() else {
        return Err("a frontmatter target starts with a property name".to_string());
    };
    let parsed = crate::parse_document(source, &VaultConfig::default());
    let mut frontmatter = frontmatter_json(&parsed);
    let before = frontmatter.get(root).cloned();
    write_path(&mut frontmatter, &target.path, value.clone())?;
    let after = frontmatter.get(root).cloned().unwrap_or(Value::Null);
    if before.as_ref() == Some(&after) {
        return Ok(None);
    }
    let yaml = serde_json::from_value::<yaml_serde::Value>(after.clone())
        .map_err(|error| error.to_string())?;
    if !after.is_object() && !after.is_array() {
        return crate::refactor::set_frontmatter_property_in_source(
            source,
            path,
            root,
            Some(&yaml),
        )
        .map_err(|error| error.to_string());
    }
    if let Some(updated) = replace_key_lines(source, root, &yaml, &frontmatter) {
        return Ok(Some(updated));
    }
    crate::refactor::set_frontmatter_property_in_source(source, path, root, Some(&yaml))
        .map_err(|error| error.to_string())
}

/// Rewrites only the lines of top-level key `key` (its own line and the
/// indented or `- ` lines under it), or appends the key, when the result
/// reads back as `expected`; `None` otherwise.
fn replace_key_lines(
    source: &str,
    key: &str,
    value: &yaml_serde::Value,
    expected: &Value,
) -> Option<String> {
    let body = source.strip_prefix("---\n")?;
    let end = body.find("\n---")? + 1;
    let yaml = &body[..end];
    let mut mapping = yaml_serde::Mapping::new();
    mapping.insert(yaml_serde::Value::String(key.to_string()), value.clone());
    let mut replacement = yaml_serde::to_string(&yaml_serde::Value::Mapping(mapping)).ok()?;
    if !replacement.ends_with('\n') {
        replacement.push('\n');
    }
    let mut lines = Vec::new();
    let mut offset = 0;
    for line in yaml.split_inclusive('\n') {
        lines.push((offset, line));
        offset += line.len();
    }
    let is_key_line = |line: &str| {
        line.strip_prefix(key)
            .is_some_and(|rest| rest.starts_with(':'))
            || line
                .strip_prefix(&format!("{}", Value::String(key.to_string())))
                .is_some_and(|rest| rest.starts_with(':'))
    };
    let new_yaml = if let Some(start) = lines.iter().position(|(_, line)| is_key_line(line)) {
        let stop = lines[start + 1..]
            .iter()
            .position(|(_, line)| {
                !(line.starts_with(' ')
                    || line.starts_with('\t')
                    || line.starts_with("- ")
                    || line.trim().is_empty())
            })
            .map_or(lines.len(), |position| start + 1 + position);
        let from = lines[start].0;
        let to = lines.get(stop).map_or(yaml.len(), |(offset, _)| *offset);
        format!("{}{replacement}{}", &yaml[..from], &yaml[to..])
    } else {
        let separator = if yaml.ends_with('\n') || yaml.is_empty() {
            ""
        } else {
            "\n"
        };
        format!("{yaml}{separator}{replacement}")
    };
    let updated = format!("---\n{new_yaml}{}", &body[end..]);
    let parsed = crate::parse_document(&updated, &VaultConfig::default());
    (frontmatter_json(&parsed) == *expected).then_some(updated)
}

/// A view's computed value and where `vulcan metabind apply` writes it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ViewWriteBack {
    pub line_number: usize,
    pub target: BindTarget,
    pub value: Value,
}

/// The persistent write-backs of the note's views, in document order:
/// frontmatter targets only, since memory is never stored.
#[must_use]
pub fn view_write_backs(fields: &[MetaBindField]) -> Vec<ViewWriteBack> {
    fields
        .iter()
        .filter_map(|field| {
            let target = match &field.parsed {
                Some(MetaBindDeclaration::View(view)) => view.write_target.clone()?,
                Some(MetaBindDeclaration::JsView(view)) => view.save_to.clone()?,
                _ => return None,
            };
            if target.storage != MetaBindStorage::Frontmatter {
                return None;
            }
            let MetaBindOutcome::View { value, .. } = &field.outcome else {
                return None;
            };
            let value = value.clone();
            Some(ViewWriteBack {
                line_number: field.line_number,
                target,
                value,
            })
        })
        .collect()
}

/// The note's fields for `vulcan metabind` and editors.
#[must_use]
pub fn fields_in_source<S: std::hash::BuildHasher>(
    config: &VaultConfig,
    note_index: Option<&HashMap<String, NoteRecord, S>>,
    source_path: Option<&str>,
    source: &str,
    env: &MetaBindEnv<'_>,
) -> Vec<MetaBindField> {
    let mut config = config.clone();
    config.metabind.enabled = true;
    let parsed = crate::parse_document(source, &config);
    evaluate_note_fields(&config, note_index, source_path, &parsed, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> MetaBindConfig {
        MetaBindConfig::default()
    }

    fn render(source: &str) -> Vec<MetaBindField> {
        fields_in_source::<std::collections::hash_map::RandomState>(
            &VaultConfig::default(),
            None,
            Some("Note.md"),
            source,
            &MetaBindEnv::default(),
        )
    }

    #[test]
    fn bind_targets_parse_storage_files_and_paths() {
        let target = BindTarget::parse(r#"Other Note#stats.hp[0]["max hp"]"#).expect("parse");
        assert_eq!(target.storage, MetaBindStorage::Frontmatter);
        assert_eq!(target.file.as_deref(), Some("Other Note"));
        assert_eq!(
            target.path,
            vec![
                PathSegment::Key("stats".into()),
                PathSegment::Key("hp".into()),
                PathSegment::Index(0),
                PathSegment::Key("max hp".into()),
            ]
        );
        assert_eq!(target.to_string(), r#"Other Note#stats.hp[0]["max hp"]"#);
        assert_eq!(
            BindTarget::parse("memory^count").expect("memory").storage,
            MetaBindStorage::Memory
        );
        assert!(BindTarget::parse("max hp").is_err());
        assert!(BindTarget::parse("nowhere^x").is_err());
    }

    #[test]
    fn declarations_parse_arguments_templates_and_views() {
        let MetaBindDeclaration::Input(input) = parse_declaration(
            "INPUT[slider(minValue(1), maxValue(10), class(a), class(b)):level]",
            &config(),
        )
        .expect("input") else {
            panic!("not an input")
        };
        assert_eq!(input.input_type, "slider");
        assert_eq!(input.arguments.len(), 4);
        assert_eq!(input.target.expect("target").to_string(), "level");

        let mut with_template = config();
        with_template
            .input_field_templates
            .push(crate::config::MetaBindInputTemplate {
                name: "mood".into(),
                declaration: "INPUT[inlineSelect(option(1, 'bad'), option(2, 'good')):mood]".into(),
            });
        let MetaBindDeclaration::Input(input) =
            parse_declaration("INPUT[mood][:today.mood]", &with_template).expect("template")
        else {
            panic!("not an input")
        };
        assert_eq!(input.input_type, "inlineSelect");
        assert_eq!(input.template.as_deref(), Some("mood"));
        assert_eq!(input.target.expect("target").to_string(), "today.mood");

        let MetaBindDeclaration::View(view) = parse_declaration(
            r"VIEW[{a} \[x\] + {b}][text(renderMarkdown):sum]",
            &config(),
        )
        .expect("view") else {
            panic!("not a view")
        };
        assert_eq!(view.view_type, "text");
        assert_eq!(view.parts.len(), 3);
        assert_eq!(
            view.parts[1],
            ViewPart::Text {
                text: " [x] + ".into()
            }
        );
        assert_eq!(view.write_target.expect("write").to_string(), "sum");
        assert!(parse_declaration("INPUT[toggle:", &config()).is_err());
    }

    #[test]
    fn literals_follow_meta_binds_rules() {
        assert_eq!(parse_literal("NULL"), Value::Null);
        assert_eq!(parse_literal("true"), Value::Bool(true));
        assert_eq!(parse_literal("True"), Value::String("True".into()));
        assert_eq!(parse_literal("-3"), Value::from(-3));
        assert_eq!(parse_literal("2.5"), serde_json::json!(2.5));
        assert_eq!(parse_literal("1e3"), Value::String("1e3".into()));
    }

    #[test]
    fn inputs_show_current_values_and_defaults() {
        let fields = render(concat!(
            "---\ndone: true\nlevel: 3\nmood: 2\nstats:\n  tags: [a, b]\n---\n",
            "Done `INPUT[toggle:done]`, level `INPUT[slider(maxValue(10)):level]`, ",
            "mood `INPUT[inlineSelect(option(1, bad), option(2, good)):mood]`, ",
            "tags `INPUT[inlineList:stats.tags]`, missing `INPUT[number(defaultValue(7)):nope]`.\n",
        ));
        let shown = fields
            .iter()
            .map(|field| field_markdown(field).1)
            .collect::<Vec<_>>();
        assert_eq!(shown, vec!["[x]", "3 / 10", "good", "a, b", "7"]);
    }

    #[test]
    fn date_inputs_default_to_now_when_the_render_has_a_clock() {
        let source =
            "---\nset: 2020-01-02\n---\n`INPUT[date:missing]` `INPUT[time:t]` `INPUT[date:set]`\n";
        let shown = |env: &MetaBindEnv<'_>| {
            fields_in_source::<std::collections::hash_map::RandomState>(
                &VaultConfig::default(),
                None,
                Some("Note.md"),
                source,
                env,
            )
            .iter()
            .map(|field| field_markdown(field).1)
            .collect::<Vec<_>>()
        };
        let reproducible = shown(&MetaBindEnv::default());
        assert_eq!(reproducible[2], "2020-01-02");
        assert!(
            reproducible[0].is_empty() || reproducible[0] == "null",
            "{reproducible:?}"
        );

        let now = shown(&MetaBindEnv {
            now_ms: Some(1_700_000_000_000),
            ..MetaBindEnv::default()
        });
        assert!(now[0].parse::<jiff::civil::Date>().is_ok(), "{now:?}");
        assert!(
            now[1].len() == 5 && now[1].parse::<jiff::civil::Time>().is_ok(),
            "{now:?}"
        );
        assert_eq!(now[2], "2020-01-02", "a stored value wins over now");
    }

    #[test]
    fn js_views_parse_bindings_save_targets_and_code() {
        let view = parse_js_view(
            "{number1} as n1\n{Other#list} and children as items\nsave to {result}\nhidden\n---\nreturn 1;\n",
        )
        .expect("view");
        assert_eq!(view.bindings.len(), 2);
        assert_eq!(view.bindings[1].0, "items");
        assert_eq!(view.bindings[1].1.to_string(), "Other#list");
        assert_eq!(view.save_to.expect("save").to_string(), "result");
        assert!(view.hidden);
        assert_eq!(view.code, "return 1;\n");
        assert!(parse_js_view("{a} as a\nreturn 1;").is_err(), "needs ---");
        assert!(parse_js_view("{a} as not a name\n---\n").is_err());
        assert_eq!(
            script_value(Some(serde_json::json!({ "__vulcanMarkdown": "**x**" }))),
            MetaBindScriptValue::Markdown("**x**".to_string())
        );
    }

    #[test]
    fn moment_date_formats_follow_the_tokens() {
        let date = jiff::civil::date(2026, 3, 7);
        assert_eq!(moment_date(date, "YYYY-MM-DD"), "2026-03-07");
        assert_eq!(moment_date(date, "D.M.YY"), "7.3.26");
        assert_eq!(moment_date(date, "[Day] DD/MM/YYYY"), "Day 07/03/2026");
        assert_eq!(moment_date(date, ""), "2026-03-07");
    }

    #[test]
    fn block_only_inputs_fail_inline_but_work_in_blocks() {
        let fields = render(
            "`INPUT[select(option(a)):x]`\n\n```meta-bind\nINPUT[select(option(a), option(b)):x]\n```\n",
        );
        assert!(matches!(fields[0].outcome, MetaBindOutcome::Error { .. }));
        assert!(matches!(fields[1].outcome, MetaBindOutcome::Input { .. }));
        assert!(fields[1].block);
    }

    #[test]
    fn text_link_and_hidden_views_compute_in_order() {
        let fields = render(concat!(
            "---\nname: Ada\nfriend: Grace\nempty: null\n---\n",
            "`VIEW[Hi {name}, {empty}!][text]` `VIEW[{friend}][link]` ",
            "`VIEW[{name}][text(hidden):memory^copy]` `VIEW[copy: {memory^copy}][text]`\n",
        ));
        let shown = fields
            .iter()
            .map(|field| field_markdown(field).1)
            .collect::<Vec<_>>();
        assert_eq!(shown, vec!["Hi Ada, null!", "[[Grace]]", "", "copy: Ada"]);
    }

    #[test]
    fn excluded_folders_show_a_notice() {
        let config = VaultConfig::default();
        let fields = fields_in_source::<std::collections::hash_map::RandomState>(
            &config,
            None,
            Some("templates/Daily.md"),
            "`INPUT[toggle:done]`",
            &MetaBindEnv::default(),
        );
        assert!(matches!(fields[0].outcome, MetaBindOutcome::Notice { .. }));
    }

    #[test]
    fn inputs_validate_values_before_writing() {
        let fields = render(concat!(
            "`INPUT[slider(minValue(1), maxValue(5)):level]` ",
            "`INPUT[inlineSelect(option(low), option(high)):p]` `INPUT[toggle:done]`\n",
        ));
        let MetaBindOutcome::Input { settings, .. } = &fields[0].outcome else {
            panic!()
        };
        assert_eq!(settings.parse_value("4"), Ok(Value::from(4)));
        assert!(settings.parse_value("9").is_err());
        let MetaBindOutcome::Input { settings, .. } = &fields[1].outcome else {
            panic!()
        };
        assert_eq!(
            settings.parse_value("high"),
            Ok(Value::String("high".into()))
        );
        assert!(settings.parse_value("medium").is_err());
        let MetaBindOutcome::Input { settings, .. } = &fields[2].outcome else {
            panic!()
        };
        assert_eq!(settings.parse_value("on"), Ok(Value::Bool(true)));
    }

    #[test]
    fn writes_create_nested_values_and_keep_other_keys() {
        let source = "---\ntitle: \"Kept  as is\"\nstats:\n  hp: 3\n---\nBody\n";
        let target = BindTarget::parse("stats.list[1]").expect("target");
        let updated = write_target_in_source(source, "Note.md", &target, &Value::from(5))
            .expect("write")
            .expect("changed");
        assert!(updated.contains("title: \"Kept  as is\""), "{updated}");
        let parsed = crate::parse_document(&updated, &VaultConfig::default());
        assert_eq!(
            frontmatter_json(&parsed)["stats"],
            serde_json::json!({"hp": 3, "list": [null, 5]})
        );
        let top = BindTarget::parse("done").expect("target");
        let toggled = write_target_in_source(source, "Note.md", &top, &Value::Bool(true))
            .expect("write")
            .expect("changed");
        assert!(toggled.contains("done: true"));
        assert_eq!(
            write_target_in_source(&toggled, "Note.md", &top, &Value::Bool(true)).expect("write"),
            None
        );
    }
}
