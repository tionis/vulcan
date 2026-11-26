//! Meta Bind buttons for `vulcan metabind press`: finding the pressed
//! button, its actions, and the note edits of the text actions
//! (`replaceInNote`, `insertIntoNote`, `regexpReplaceInNote`,
//! `replaceSelf`). The workflow that runs the actions, including the ones
//! that write other notes or run JavaScript, is `vulcan_app::metabind`.
//! Reimplemented from Meta Bind's documentation; none of the plugin's
//! GPL-3.0 code is used.

use std::fmt::Write as _;
use std::ops::Range;

use serde_json::Value;

use super::{MetaBindConfig, MetaBindSourceKind, ParsedDocument};

/// Which button to press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ButtonSelector {
    /// A button `id`: a `meta-bind-button` block in the note, else a
    /// configured button template.
    Id(String),
    /// The `meta-bind-button` block on (or spanning) a one-based line, for
    /// buttons without an id.
    Line(usize),
}

/// A button found in a note or the button templates.
#[derive(Debug, Clone, PartialEq)]
pub struct PressableButton {
    /// The button's configuration (label, actions, ...).
    pub config: Value,
    /// The block that declares it: its byte range and its one-based first
    /// and last (fence) lines. `None` for a button template.
    pub block: Option<ButtonBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonBlock {
    pub byte_range: Range<usize>,
    pub first_line: usize,
    pub last_line: usize,
}

impl PressableButton {
    #[must_use]
    pub fn label(&self) -> &str {
        self.config
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The button's actions: `actions`, else the single `action`.
    pub fn actions(&self) -> Result<Vec<Value>, String> {
        if let Some(actions) = self.config.get("actions") {
            return actions
                .as_array()
                .cloned()
                .ok_or_else(|| "`actions` must be a list".to_string());
        }
        match self.config.get("action") {
            Some(action @ Value::Object(_)) => Ok(vec![action.clone()]),
            Some(_) => Err("`action` must be a mapping".to_string()),
            None => Err("the button has no `action` or `actions`".to_string()),
        }
    }
}

fn yaml_json(text: &str) -> Result<Value, String> {
    let yaml = yaml_serde::from_str::<yaml_serde::Value>(text)
        .map_err(|error| format!("invalid button YAML: {error}"))?;
    serde_json::to_value(yaml).map_err(|error| error.to_string())
}

/// Finds the button `selector` names in `parsed` (the note's `source`) or,
/// by id, among the configured button templates.
pub fn find_button(
    parsed: &ParsedDocument,
    source: &str,
    config: &MetaBindConfig,
    selector: &ButtonSelector,
) -> Result<PressableButton, String> {
    for raw in &parsed.meta_bind_fields {
        if raw.kind != MetaBindSourceKind::ButtonBlock {
            continue;
        }
        let block = ButtonBlock {
            byte_range: raw.byte_range.clone(),
            first_line: raw.line_number,
            last_line: last_line(source, &raw.byte_range),
        };
        let matches = match selector {
            ButtonSelector::Line(line) => (block.first_line..=block.last_line).contains(line),
            ButtonSelector::Id(_) => true,
        };
        if !matches {
            continue;
        }
        let button = yaml_json(&raw.text)?;
        if let ButtonSelector::Id(id) = selector {
            if button.get("id").and_then(Value::as_str) != Some(id.as_str()) {
                continue;
            }
        }
        return Ok(PressableButton {
            config: button,
            block: Some(block),
        });
    }
    if let ButtonSelector::Id(id) = selector {
        if let Some(button) = config
            .button_templates
            .iter()
            .find(|button| button.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            return Ok(PressableButton {
                config: button.clone(),
                block: None,
            });
        }
        return Err(format!(
            "no button with id `{id}` in the note or the button templates"
        ));
    }
    Err("no meta-bind-button block on that line".to_string())
}

/// The one-based line of the last character of `range` (a block's closing
/// fence).
fn last_line(source: &str, range: &Range<usize>) -> usize {
    let end = source
        .get(..range.end)
        .map_or(range.end, |text| text.trim_end_matches(['\n', '\r']).len());
    source
        .get(..end)
        .map_or(1, |text| text.matches('\n').count() + 1)
}

/// A line number as Meta Bind's line actions take it: a number, or
/// `selfStart`/`selfEnd` with an optional `+ n`/`- n`.
pub fn line_number(value: &Value, block: Option<&ButtonBlock>) -> Result<usize, String> {
    if let Some(number) = value.as_u64() {
        return usize::try_from(number).map_err(|error| error.to_string());
    }
    let text = value
        .as_str()
        .ok_or_else(|| format!("`{value}` is not a line number"))?
        .trim();
    if let Ok(number) = text.parse::<usize>() {
        return Ok(number);
    }
    let (base, rest) = if let Some(rest) = text.strip_prefix("selfStart") {
        ("selfStart", rest)
    } else if let Some(rest) = text.strip_prefix("selfEnd") {
        ("selfEnd", rest)
    } else {
        return Err(format!(
            "`{text}` is not a line number or `selfStart`/`selfEnd` expression"
        ));
    };
    let block = block.ok_or_else(|| {
        format!("`{base}` needs a button declared in the note, not a button template")
    })?;
    let start = if base == "selfStart" {
        block.first_line
    } else {
        block.last_line
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(start);
    }
    let (sign, amount) = rest.split_at(1);
    let amount = amount
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("`{text}` is not a line number expression"))?;
    match sign {
        "+" => Ok(start + amount),
        "-" => start
            .checked_sub(amount)
            .ok_or_else(|| format!("`{text}` is before the first line")),
        _ => Err(format!("`{text}` is not a line number expression")),
    }
}

fn split_lines(source: &str) -> (Vec<&str>, bool) {
    let trailing = source.ends_with('\n');
    let body = source.strip_suffix('\n').unwrap_or(source);
    let lines = if source.is_empty() {
        Vec::new()
    } else {
        body.split('\n').collect()
    };
    (lines, trailing)
}

fn join_lines(lines: &[&str], trailing: bool) -> String {
    let mut text = lines.join("\n");
    if trailing && !lines.is_empty() {
        text.push('\n');
    }
    text
}

/// Replaces lines `from..=to` (one-based) with `replacement`.
pub fn replace_lines(
    source: &str,
    from: usize,
    to: usize,
    replacement: &str,
) -> Result<String, String> {
    let (lines, trailing) = split_lines(source);
    if from == 0 || from > to || to > lines.len() {
        return Err(format!(
            "lines {from} to {to} are not within the note's {} lines",
            lines.len()
        ));
    }
    let mut updated = lines[..from - 1].to_vec();
    updated.extend(replacement.split('\n'));
    updated.extend_from_slice(&lines[to..]);
    Ok(join_lines(&updated, trailing))
}

/// Inserts `value` so that it starts at one-based `line`; one past the last
/// line appends it.
pub fn insert_at_line(source: &str, line: usize, value: &str) -> Result<String, String> {
    let (lines, trailing) = split_lines(source);
    if line == 0 || line > lines.len() + 1 {
        return Err(format!(
            "line {line} is not within the note's {} lines",
            lines.len()
        ));
    }
    let mut updated = lines[..line - 1].to_vec();
    updated.extend(value.split('\n'));
    updated.extend_from_slice(&lines[line - 1..]);
    Ok(join_lines(&updated, trailing || line <= lines.len()))
}

/// Replaces the button's own block with `replacement`.
pub fn replace_block(
    source: &str,
    block: &ButtonBlock,
    replacement: &str,
) -> Result<String, String> {
    let range = &block.byte_range;
    let original = source
        .get(range.clone())
        .ok_or_else(|| "the button block moved".to_string())?;
    let mut text = replacement.to_string();
    if original.ends_with('\n') && !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(format!(
        "{}{text}{}",
        &source[..range.start],
        &source[range.end..]
    ))
}

/// Applies a JavaScript-style regular expression replacement: `flags` as
/// in JavaScript (`g` replaces every match, `i`, `m`, `s`; default `g`) and
/// `$1`, `$<name>`, `$&`, `$$` in `replacement`. Lookaround and
/// backreferences in the pattern are not supported.
pub fn regexp_replace(
    source: &str,
    pattern: &str,
    flags: Option<&str>,
    replacement: &str,
) -> Result<String, String> {
    let flags = flags.unwrap_or("g");
    let mut inline = String::new();
    for flag in flags.chars() {
        match flag {
            'i' | 'm' | 's' => inline.push(flag),
            'g' | 'u' | 'd' => {}
            other => {
                return Err(format!(
                    "regular expression flag `{other}` is not supported"
                ))
            }
        }
    }
    let pattern = if inline.is_empty() {
        pattern.to_string()
    } else {
        format!("(?{inline}){pattern}")
    };
    let regex = regex::Regex::new(&pattern).map_err(|error| error.to_string())?;
    let replacement = js_replacement(replacement);
    Ok(if flags.contains('g') {
        regex.replace_all(source, replacement.as_str()).into_owned()
    } else {
        regex.replace(source, replacement.as_str()).into_owned()
    })
}

/// A JavaScript replacement string in the `regex` crate's syntax.
fn js_replacement(replacement: &str) -> String {
    let mut out = String::new();
    let mut chars = replacement.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '$' {
            out.push(character);
            continue;
        }
        match chars.peek().copied() {
            Some('$') => {
                chars.next();
                out.push_str("$$");
            }
            Some('&') => {
                chars.next();
                out.push_str("${0}");
            }
            Some(digit) if digit.is_ascii_digit() => {
                let mut group = String::new();
                while let Some(digit) = chars.peek().copied().filter(char::is_ascii_digit) {
                    group.push(digit);
                    chars.next();
                    if group.len() == 2 {
                        break;
                    }
                }
                let _ = write!(out, "${{{group}}}");
            }
            Some('<') => {
                let rest: String = chars.clone().collect();
                if let Some(end) = rest.find('>') {
                    let _ = write!(out, "${{{}}}", &rest[1..end]);
                    for _ in 0..=end {
                        chars.next();
                    }
                } else {
                    out.push_str("$$");
                }
            }
            _ => out.push_str("$$"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VaultConfig;

    const NOTE: &str = "# Title\n\nother\ntext\ntada\n\n```meta-bind-button\nlabel: Go\nid: go\naction:\n  type: sleep\n  ms: 1\n```\nafter\n";

    fn find(selector: &ButtonSelector) -> Result<PressableButton, String> {
        let config = VaultConfig::default();
        let parsed = crate::parse_document(NOTE, &config);
        find_button(&parsed, NOTE, &config.metabind, selector)
    }

    #[test]
    fn buttons_are_found_by_id_line_or_template() {
        let button = find(&ButtonSelector::Id("go".to_string())).expect("button");
        assert_eq!(button.label(), "Go");
        let block = button.block.clone().expect("block");
        assert_eq!((block.first_line, block.last_line), (7, 13));
        assert_eq!(button.actions().expect("actions").len(), 1);
        assert_eq!(find(&ButtonSelector::Line(9)).expect("by line"), button);
        assert!(find(&ButtonSelector::Line(3)).is_err());
        assert!(find(&ButtonSelector::Id("nope".to_string())).is_err());

        let mut config = VaultConfig::default();
        config.metabind.button_templates = vec![serde_json::json!({
            "id": "tpl", "label": "T", "action": { "type": "sleep", "ms": 1 }
        })];
        let parsed = crate::parse_document(NOTE, &config);
        let template = find_button(
            &parsed,
            NOTE,
            &config.metabind,
            &ButtonSelector::Id("tpl".to_string()),
        )
        .expect("template");
        assert!(template.block.is_none());
    }

    #[test]
    fn line_expressions_and_line_edits_follow_meta_bind() {
        let block = find(&ButtonSelector::Id("go".to_string()))
            .expect("button")
            .block
            .expect("block");
        assert_eq!(line_number(&Value::from(3), Some(&block)), Ok(3));
        assert_eq!(
            line_number(&Value::from("selfEnd + 1"), Some(&block)),
            Ok(14)
        );
        assert_eq!(
            line_number(&Value::from("selfStart-2"), Some(&block)),
            Ok(5)
        );
        assert!(line_number(&Value::from("selfStart"), None).is_err());

        assert_eq!(
            replace_lines(NOTE, 3, 5, "some\ntext\nwow").expect("replace"),
            NOTE.replace("other\ntext\ntada", "some\ntext\nwow")
        );
        assert_eq!(
            insert_at_line("a\nb\n", 2, "x").expect("insert"),
            "a\nx\nb\n"
        );
        assert_eq!(
            insert_at_line("a\nb\n", 3, "x").expect("append"),
            "a\nb\nx\n"
        );
        assert!(replace_lines(NOTE, 3, 99, "x").is_err());
        let replaced = replace_block(NOTE, &block, "no longer a button").expect("self");
        assert!(
            replaced.ends_with("tada\n\nno longer a button\nafter\n"),
            "{replaced}"
        );
    }

    #[test]
    fn regexp_replacements_use_javascript_syntax() {
        let source = "Insert below:\ni am inserted\ni am inserted\nend\n";
        assert_eq!(
            regexp_replace(
                source,
                "Insert below:\n(i am inserted\n)+",
                None,
                "Insert below:\n"
            )
            .expect("replace"),
            "Insert below:\nend\n"
        );
        assert_eq!(
            regexp_replace("a1 b2", r"(\w)(\d)", Some("g"), "$2$1-$&-$$").expect("groups"),
            "1a-a1-$ 2b-b2-$"
        );
        assert_eq!(
            regexp_replace("Aa", "a", Some(""), "x").expect("first only"),
            "Ax"
        );
        assert_eq!(regexp_replace("Aa", "a", Some("gi"), "x").expect("i"), "xx");
        assert_eq!(
            regexp_replace("k=v", "(?<key>k)=(?<val>v)", None, "$<val>=$<key>").expect("named"),
            "v=k"
        );
    }
}
