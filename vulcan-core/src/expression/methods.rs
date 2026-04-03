use serde_json::Value;

use crate::expression::ast::Expr;
use crate::expression::eval::{
    as_number, compare_values, evaluate, is_truthy, number_to_value, value_to_display, EvalContext,
};
use crate::expression::functions::{date_components, format_date, parse_date_like_string};

pub fn call_method(
    receiver: &Value,
    method: &str,
    args: &[Expr],
    ctx: &EvalContext,
) -> Result<Value, String> {
    // Universal methods (any type)
    match method {
        "isTruthy" => return Ok(Value::Bool(is_truthy(receiver))),
        "isType" => {
            let expected = eval_arg(args, 0, ctx)?;
            return Ok(Value::Bool(
                expected
                    .as_str()
                    .is_some_and(|s| value_matches_type_name(receiver, s)),
            ));
        }
        "toString" => return Ok(Value::String(value_to_display(receiver))),
        _ => {}
    }

    match receiver {
        Value::String(s) => string_method(s, method, args, ctx),
        Value::Number(n) => number_method(n.as_f64().unwrap_or(0.0), method, args, ctx),
        Value::Array(arr) => array_method(arr, method, args, ctx),
        Value::Object(map) => object_method(map, method, args, ctx),
        Value::Bool(_) => match method {
            "isEmpty" => Ok(Value::Bool(false)),
            _ => Err(unknown_method_error(method, "boolean")),
        },
        Value::Null => match method {
            "isEmpty" => Ok(Value::Bool(true)),
            _ => Err(unknown_method_error(method, "null")),
        },
    }
}

// ── String methods ───────────────────────────────────────────────────

#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn string_method(s: &str, method: &str, args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    match method {
        "contains" => {
            let needle = eval_string_arg(args, 0, ctx)?;
            Ok(Value::Bool(s.contains(&needle)))
        }
        "containsAll" => {
            let values = eval_all_string_args(args, ctx)?;
            Ok(Value::Bool(values.iter().all(|v| s.contains(v.as_str()))))
        }
        "containsAny" => {
            let values = eval_all_string_args(args, ctx)?;
            Ok(Value::Bool(values.iter().any(|v| s.contains(v.as_str()))))
        }
        "startsWith" => {
            let prefix = eval_string_arg(args, 0, ctx)?;
            Ok(Value::Bool(s.starts_with(&prefix)))
        }
        "endsWith" => {
            let suffix = eval_string_arg(args, 0, ctx)?;
            Ok(Value::Bool(s.ends_with(&suffix)))
        }
        "isEmpty" => Ok(Value::Bool(s.is_empty())),
        "lower" => Ok(Value::String(s.to_lowercase())),
        "title" => Ok(Value::String(title_case(s))),
        "trim" => Ok(Value::String(s.trim().to_string())),
        "reverse" => Ok(Value::String(s.chars().rev().collect())),
        "repeat" => {
            let count = eval_number_arg(args, 0, ctx)? as usize;
            Ok(Value::String(s.repeat(count)))
        }
        "replace" => {
            let pattern = eval_string_arg(args, 0, ctx)?;
            let replacement = eval_string_arg(args, 1, ctx)?;
            Ok(Value::String(s.replace(&pattern, &replacement)))
        }
        "slice" => {
            let start = eval_number_arg(args, 0, ctx)? as usize;
            let chars: Vec<char> = s.chars().collect();
            let end = if args.len() > 1 {
                eval_number_arg(args, 1, ctx)? as usize
            } else {
                chars.len()
            };
            let start = start.min(chars.len());
            let end = end.min(chars.len());
            Ok(Value::String(chars[start..end].iter().collect()))
        }
        "split" => {
            let separator = eval_string_arg(args, 0, ctx)?;
            let parts: Vec<Value> = s
                .split(&separator)
                .map(|p| Value::String(p.to_string()))
                .collect();
            let parts = if args.len() > 1 {
                let n = eval_number_arg(args, 1, ctx)? as usize;
                parts.into_iter().take(n).collect()
            } else {
                parts
            };
            Ok(Value::Array(parts))
        }
        "matches" => {
            // Accepts a regex value (stored as "/pattern/flags") or a plain string pattern.
            // We implement a simple substring match (case-insensitive if 'i' flag present).
            let pattern_val = eval_arg(args, 0, ctx)?;
            let (pattern, case_insensitive) = match &pattern_val {
                Value::String(p) if p.starts_with('/') => {
                    // Stored regex format: /pattern/flags
                    if let Some(end) = p.rfind('/') {
                        if end > 0 {
                            let flags = &p[end + 1..];
                            let pat = &p[1..end];
                            (pat.to_string(), flags.contains('i'))
                        } else {
                            (p.clone(), false)
                        }
                    } else {
                        (p.clone(), false)
                    }
                }
                Value::String(p) => (p.clone(), false),
                _ => return Ok(Value::Bool(false)),
            };
            let matched = if case_insensitive {
                s.to_lowercase().contains(&pattern.to_lowercase())
            } else {
                s.contains(&pattern)
            };
            Ok(Value::Bool(matched))
        }
        // Link methods — available on any string that looks like a wikilink
        "asFile" => {
            use crate::expression::eval::{
                note_to_file_object, parse_wikilink_target, resolve_note_reference,
            };
            let target = parse_wikilink_target(s);
            if let Some(lookup) = ctx.note_lookup {
                if let Some(note) = resolve_note_reference(lookup, &ctx.note.document_path, &target)
                {
                    return Ok(note_to_file_object(note));
                }
            }
            Ok(Value::Null)
        }
        "linksTo" => {
            use crate::expression::eval::{parse_wikilink_target, resolve_note_reference};
            // `s` is the source link; arg 0 is the target file (link string or filename)
            let source_name = parse_wikilink_target(s);
            let file_arg = eval_arg(args, 0, ctx)?;
            let target_name = match &file_arg {
                Value::String(fs) => parse_wikilink_target(fs),
                _ => return Ok(Value::Bool(false)),
            };
            if let Some(lookup) = ctx.note_lookup {
                if let Some(source_note) =
                    resolve_note_reference(lookup, &ctx.note.document_path, &source_name)
                {
                    let found = source_note
                        .links
                        .iter()
                        .any(|l| parse_wikilink_target(l) == target_name);
                    return Ok(Value::Bool(found));
                }
            }
            Ok(Value::Bool(false))
        }
        // Date methods on strings that look like dates
        "format" | "date" | "time" | "relative" | "year" | "month" | "day" | "hour" | "minute"
        | "second" | "millisecond" | "weekday" | "week" | "weekyear" => {
            // Try to parse as date
            if let Some(ms) = parse_date_like_string(s) {
                return date_method(ms, method, args, ctx);
            }
            Ok(Value::Null)
        }
        _ => Err(unknown_method_error(method, "string")),
    }
}

fn title_case(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for ch in s.chars() {
        if ch.is_whitespace() {
            capitalize_next = true;
            result.push(ch);
        } else if capitalize_next {
            result.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }
    result
}

// ── Number methods ───────────────────────────────────────────────────

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn number_method(n: f64, method: &str, args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    match method {
        "abs" => Ok(number_to_value(n.abs())),
        "ceil" => Ok(number_to_value(n.ceil())),
        "floor" => Ok(number_to_value(n.floor())),
        "round" => {
            if args.is_empty() {
                Ok(number_to_value(n.round()))
            } else {
                let digits = eval_number_arg(args, 0, ctx)?;
                let factor = 10_f64.powi(digits as i32);
                Ok(number_to_value((n * factor).round() / factor))
            }
        }
        "toFixed" => {
            let precision = eval_number_arg(args, 0, ctx)? as usize;
            Ok(Value::String(format!("{n:.precision$}")))
        }
        "isEmpty" => Ok(Value::Bool(false)),
        // Treat numbers as date timestamps for date methods
        "format" | "date" | "time" | "relative" | "year" | "month" | "day" | "hour" | "minute"
        | "second" | "millisecond" | "weekday" | "week" | "weekyear" => {
            date_method(n as i64, method, args, ctx)
        }
        _ => Err(unknown_method_error(method, "number")),
    }
}

// ── Date methods ─────────────────────────────────────────────────────

fn date_method(ms: i64, method: &str, args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let (year, month, day, hour, minute, second, millisecond) = date_components(ms);
    let weekday = crate::expression::functions::date_field_value(ms, "weekday")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let week = crate::expression::functions::date_field_value(ms, "week")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    let weekyear = crate::expression::functions::date_field_value(ms, "weekyear")
        .and_then(|value| value.as_i64())
        .unwrap_or(year);

    match method {
        "year" => Ok(Value::Number(year.into())),
        "month" => Ok(Value::Number(month.into())),
        "day" => Ok(Value::Number(day.into())),
        "hour" => Ok(Value::Number(hour.into())),
        "minute" => Ok(Value::Number(minute.into())),
        "second" => Ok(Value::Number(second.into())),
        "millisecond" => Ok(Value::Number(millisecond.into())),
        "weekday" => Ok(Value::Number(weekday.into())),
        "week" => Ok(Value::Number(week.into())),
        "weekyear" => Ok(Value::Number(weekyear.into())),
        "format" => {
            let fmt = eval_string_arg(args, 0, ctx)?;
            Ok(Value::String(format_date(ms, &fmt)))
        }
        "date" => {
            // Truncate to start of day
            let day_ms = 86_400_000_i64;
            let truncated = (ms / day_ms) * day_ms;
            Ok(Value::Number(truncated.into()))
        }
        "time" => Ok(Value::String(format!("{hour:02}:{minute:02}:{second:02}"))),
        "relative" => {
            let diff_ms = ctx.now_ms - ms;
            let diff_seconds = diff_ms.abs() / 1000;
            let label = if diff_seconds < 60 {
                "just now".to_string()
            } else if diff_seconds < 3600 {
                let mins = diff_seconds / 60;
                format!("{mins} minute{}", if mins == 1 { "" } else { "s" })
            } else if diff_seconds < 86400 {
                let hours = diff_seconds / 3600;
                format!("{hours} hour{}", if hours == 1 { "" } else { "s" })
            } else {
                let days = diff_seconds / 86400;
                format!("{days} day{}", if days == 1 { "" } else { "s" })
            };
            if diff_ms < 0 {
                Ok(Value::String(format!("in {label}")))
            } else {
                Ok(Value::String(format!("{label} ago")))
            }
        }
        "isEmpty" => Ok(Value::Bool(false)),
        _ => Err(unknown_method_error(method, "date")),
    }
}

fn value_matches_type_name(receiver: &Value, expected: &str) -> bool {
    match receiver {
        Value::Null => expected == "null",
        Value::Bool(_) => expected == "boolean",
        Value::Number(_) => expected == "number",
        Value::String(_) => expected == "string",
        Value::Array(_) => matches!(expected, "array" | "list"),
        Value::Object(_) => expected == "object",
    }
}

// ── Array methods ────────────────────────────────────────────────────

#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn array_method(
    arr: &[Value],
    method: &str,
    args: &[Expr],
    ctx: &EvalContext,
) -> Result<Value, String> {
    match method {
        "contains" => {
            let needle = eval_arg(args, 0, ctx)?;
            Ok(Value::Bool(arr.iter().any(|v| values_equal(v, &needle))))
        }
        "containsAll" => {
            let needles = eval_all_args(args, ctx)?;
            Ok(Value::Bool(
                needles
                    .iter()
                    .all(|n| arr.iter().any(|v| values_equal(v, n))),
            ))
        }
        "containsAny" => {
            let needles = eval_all_args(args, ctx)?;
            Ok(Value::Bool(
                needles
                    .iter()
                    .any(|n| arr.iter().any(|v| values_equal(v, n))),
            ))
        }
        "isEmpty" => Ok(Value::Bool(arr.is_empty())),
        "join" => {
            let sep = if args.is_empty() {
                ", ".to_string()
            } else {
                eval_string_arg(args, 0, ctx)?
            };
            let joined: String = arr
                .iter()
                .map(value_to_display)
                .collect::<Vec<_>>()
                .join(&sep);
            Ok(Value::String(joined))
        }
        "flat" => {
            let depth = if args.is_empty() {
                1
            } else {
                eval_number_arg(args, 0, ctx)? as i64
            };
            Ok(Value::Array(flatten_array(arr, depth)))
        }
        "reverse" => {
            let mut result: Vec<Value> = arr.to_vec();
            result.reverse();
            Ok(Value::Array(result))
        }
        "sort" => {
            if args.is_empty() {
                let mut result: Vec<Value> = arr.to_vec();
                result.sort_by(compare_values_for_sort);
                return Ok(Value::Array(result));
            }

            let mut keyed = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let key = evaluate_callback(
                    &args[0],
                    ctx,
                    &[("value", item.clone()), ("index", Value::Number(i.into()))],
                    &[item.clone(), Value::Number(i.into())],
                )?;
                keyed.push((item.clone(), key));
            }
            keyed.sort_by(|left, right| compare_values_for_sort(&left.1, &right.1));
            Ok(Value::Array(
                keyed.into_iter().map(|(item, _)| item).collect(),
            ))
        }
        "unique" => {
            let mut result = Vec::new();
            for item in arr {
                if !result.iter().any(|v| values_equal(v, item)) {
                    result.push(item.clone());
                }
            }
            Ok(Value::Array(result))
        }
        "slice" => {
            let start = if args.is_empty() {
                0
            } else {
                eval_number_arg(args, 0, ctx)? as i64
            };
            let end = if args.len() > 1 {
                Some(eval_number_arg(args, 1, ctx)? as i64)
            } else {
                None
            };
            let (start, end) = normalize_slice_bounds(arr.len(), start, end);
            Ok(Value::Array(arr[start..end].to_vec()))
        }
        "filter" => {
            if args.is_empty() {
                return Ok(Value::Array(arr.to_vec()));
            }
            let mut result = Vec::new();
            for (i, item) in arr.iter().enumerate() {
                let keep = evaluate_callback(
                    &args[0],
                    ctx,
                    &[("value", item.clone()), ("index", Value::Number(i.into()))],
                    &[item.clone(), Value::Number(i.into())],
                )?;
                if is_truthy(&keep) {
                    result.push(item.clone());
                }
            }
            Ok(Value::Array(result))
        }
        "map" => {
            if args.is_empty() {
                return Ok(Value::Array(arr.to_vec()));
            }
            let mut result = Vec::new();
            for (i, item) in arr.iter().enumerate() {
                result.push(evaluate_callback(
                    &args[0],
                    ctx,
                    &[("value", item.clone()), ("index", Value::Number(i.into()))],
                    &[item.clone(), Value::Number(i.into())],
                )?);
            }
            Ok(Value::Array(result))
        }
        "reduce" => {
            if args.len() < 2 {
                return Ok(Value::Null);
            }
            let initial = evaluate(&args[1], ctx)?;
            let mut acc = initial;
            for (i, item) in arr.iter().enumerate() {
                acc = evaluate_callback(
                    &args[0],
                    ctx,
                    &[
                        ("acc", acc.clone()),
                        ("value", item.clone()),
                        ("index", Value::Number(i.into())),
                    ],
                    &[acc, item.clone(), Value::Number(i.into())],
                )?;
            }
            Ok(acc)
        }
        _ => Err(unknown_method_error(method, "array")),
    }
}

// ── Object methods ───────────────────────────────────────────────────

fn object_method(
    map: &serde_json::Map<String, Value>,
    method: &str,
    _args: &[Expr],
    _ctx: &EvalContext,
) -> Result<Value, String> {
    Ok(match method {
        "isEmpty" => Value::Bool(map.is_empty()),
        "keys" => Value::Array(map.keys().map(|k| Value::String(k.clone())).collect()),
        "values" => Value::Array(map.values().cloned().collect()),
        _ => return Err(unknown_method_error(method, "object")),
    })
}

// ── Helpers ──────────────────────────────────────────────────────────

fn unknown_method_error(method: &str, receiver_type: &str) -> String {
    format!("unknown method `{method}` on {receiver_type} values")
}

pub fn eval_arg(args: &[Expr], index: usize, ctx: &EvalContext) -> Result<Value, String> {
    args.get(index)
        .map_or(Ok(Value::Null), |e| evaluate(e, ctx))
}

fn eval_all_args(args: &[Expr], ctx: &EvalContext) -> Result<Vec<Value>, String> {
    args.iter().map(|e| evaluate(e, ctx)).collect()
}

pub(crate) fn evaluate_callback(
    expr: &Expr,
    ctx: &EvalContext,
    legacy_bindings: &[(&str, Value)],
    lambda_args: &[Value],
) -> Result<Value, String> {
    let mut local_ctx = EvalContext {
        note: ctx.note,
        formulas: ctx.formulas,
        now_ms: ctx.now_ms,
        time_zone: ctx.time_zone,
        locals: ctx.locals.clone(),
        note_lookup: ctx.note_lookup,
    };

    if let Expr::Lambda(params, body) = expr {
        for (index, param) in params.iter().enumerate() {
            local_ctx.locals.insert(
                param.clone(),
                lambda_args.get(index).cloned().unwrap_or(Value::Null),
            );
        }
        evaluate(body, &local_ctx)
    } else {
        for (name, value) in legacy_bindings {
            local_ctx.locals.insert((*name).to_string(), value.clone());
        }
        evaluate(expr, &local_ctx)
    }
}

fn flatten_array(values: &[Value], depth: i64) -> Vec<Value> {
    if depth <= 0 {
        return values.to_vec();
    }

    let mut result = Vec::new();
    for item in values {
        match item {
            Value::Array(inner) => result.extend(flatten_array(inner, depth - 1)),
            other => result.push(other.clone()),
        }
    }
    result
}

fn normalize_slice_bounds(length: usize, start: i64, end: Option<i64>) -> (usize, usize) {
    let len = i64::try_from(length).unwrap_or(i64::MAX);
    let normalize = |index: i64| {
        if index < 0 {
            (len + index).clamp(0, len)
        } else {
            index.clamp(0, len)
        }
    };

    let start = normalize(start);
    let end = end.map_or(len, normalize).max(start);
    (
        usize::try_from(start).unwrap_or(0),
        usize::try_from(end).unwrap_or(length),
    )
}

fn eval_string_arg(args: &[Expr], index: usize, ctx: &EvalContext) -> Result<String, String> {
    let val = eval_arg(args, index, ctx)?;
    Ok(value_to_display(&val))
}

fn eval_all_string_args(args: &[Expr], ctx: &EvalContext) -> Result<Vec<String>, String> {
    args.iter()
        .map(|e| {
            let val = evaluate(e, ctx)?;
            Ok(value_to_display(&val))
        })
        .collect()
}

fn eval_number_arg(args: &[Expr], index: usize, ctx: &EvalContext) -> Result<f64, String> {
    let val = eval_arg(args, index, ctx)?;
    Ok(as_number(&val))
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => a == b,
        _ => false,
    }
}

fn compare_values_for_sort(a: &Value, b: &Value) -> std::cmp::Ordering {
    compare_values(a, b).unwrap_or(std::cmp::Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::parse::Parser;
    use crate::properties::NoteRecord;
    use std::collections::BTreeMap;

    fn eval(input: &str) -> Value {
        let expr = Parser::new(input).unwrap().parse().unwrap();
        let note = NoteRecord {
            document_id: "note-id".to_string(),
            document_path: "folder/note.md".to_string(),
            file_name: "note".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1_700_000_000,
            file_ctime: 1_700_000_000,
            file_size: 1234,
            properties: serde_json::json!({"status": "done", "items": [1, 2, 3, 2]}),
            tags: vec![],
            links: vec![],
            starred: false,
            inlinks: vec![],
            aliases: vec![],
            frontmatter: serde_json::json!({}),
            list_items: vec![],
            tasks: vec![],
            raw_inline_expressions: vec![],
            inline_expressions: vec![],
        };
        let formulas = BTreeMap::new();
        let ctx = EvalContext::new(&note, &formulas);
        evaluate(&expr, &ctx).unwrap()
    }

    // ── String methods ──────────────────────────────────────────────

    #[test]
    fn string_contains() {
        assert_eq!(eval(r#""hello".contains("ell")"#), Value::Bool(true));
        assert_eq!(eval(r#""hello".contains("xyz")"#), Value::Bool(false));
    }

    #[test]
    fn string_starts_ends() {
        assert_eq!(eval(r#""hello".startsWith("he")"#), Value::Bool(true));
        assert_eq!(eval(r#""hello".endsWith("lo")"#), Value::Bool(true));
    }

    #[test]
    fn string_case() {
        assert_eq!(
            eval(r#""Hello World".lower()"#),
            Value::String("hello world".to_string())
        );
        assert_eq!(
            eval(r#""hello world".title()"#),
            Value::String("Hello World".to_string())
        );
    }

    #[test]
    fn string_trim() {
        assert_eq!(eval(r#""  hi  ".trim()"#), Value::String("hi".to_string()));
    }

    #[test]
    fn string_replace() {
        assert_eq!(
            eval(r#""a-b-c".replace("-", ",")"#),
            Value::String("a,b,c".to_string())
        );
    }

    #[test]
    fn string_split() {
        assert_eq!(
            eval(r#""a,b,c".split(",")"#),
            serde_json::json!(["a", "b", "c"])
        );
    }

    #[test]
    fn string_slice() {
        assert_eq!(
            eval(r#""hello".slice(1, 4)"#),
            Value::String("ell".to_string())
        );
    }

    #[test]
    fn string_reverse() {
        assert_eq!(
            eval(r#""hello".reverse()"#),
            Value::String("olleh".to_string())
        );
    }

    #[test]
    fn string_repeat() {
        assert_eq!(
            eval(r#""ab".repeat(3)"#),
            Value::String("ababab".to_string())
        );
    }

    #[test]
    fn string_is_empty() {
        assert_eq!(eval(r#""".isEmpty()"#), Value::Bool(true));
        assert_eq!(eval(r#""hi".isEmpty()"#), Value::Bool(false));
    }

    #[test]
    fn string_contains_all_any() {
        assert_eq!(eval(r#""hello".containsAll("h", "e")"#), Value::Bool(true));
        assert_eq!(eval(r#""hello".containsAny("x", "e")"#), Value::Bool(true));
    }

    // ── Number methods ──────────────────────────────────────────────

    #[test]
    fn number_abs() {
        assert_eq!(eval("(-5).abs()"), serde_json::json!(5));
    }

    #[test]
    fn number_ceil_floor() {
        assert_eq!(eval("(2.1).ceil()"), serde_json::json!(3));
        assert_eq!(eval("(2.9).floor()"), serde_json::json!(2));
    }

    #[test]
    fn number_round() {
        assert_eq!(eval("(2.5).round()"), serde_json::json!(3));
        assert_eq!(eval("(2.3333).round(2)"), serde_json::json!(2.33));
    }

    #[test]
    fn number_to_fixed() {
        assert_eq!(
            eval("(3.14159).toFixed(2)"),
            Value::String("3.14".to_string())
        );
    }

    // ── Array methods ───────────────────────────────────────────────

    #[test]
    fn array_contains() {
        assert_eq!(eval("[1, 2, 3].contains(2)"), Value::Bool(true));
        assert_eq!(eval("[1, 2, 3].contains(4)"), Value::Bool(false));
    }

    #[test]
    fn array_join() {
        assert_eq!(
            eval(r#"[1, 2, 3].join(",")"#),
            Value::String("1,2,3".to_string())
        );
        assert_eq!(
            eval(r#"["a", "b", "c"].join()"#),
            Value::String("a, b, c".to_string())
        );
    }

    #[test]
    fn array_reverse() {
        assert_eq!(eval("[1, 2, 3].reverse()"), serde_json::json!([3, 2, 1]));
    }

    #[test]
    fn array_sort() {
        assert_eq!(eval("[3, 1, 2].sort()"), serde_json::json!([1, 2, 3]));
        assert_eq!(
            eval("[1, 2, 3].sort((k) => 0 - k)"),
            serde_json::json!([3, 2, 1])
        );
    }

    #[test]
    fn array_unique() {
        assert_eq!(eval("[1, 2, 2, 3].unique()"), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn array_flat() {
        assert_eq!(eval("[[1, 2], [3]].flat()"), serde_json::json!([1, 2, 3]));
        assert_eq!(eval("[1, [2, [3]]].flat(2)"), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn array_slice() {
        assert_eq!(eval("[1, 2, 3, 4].slice(1, 3)"), serde_json::json!([2, 3]));
        assert_eq!(eval("[1, 2, 3, 4, 5].slice(-2)"), serde_json::json!([4, 5]));
    }

    #[test]
    fn array_filter() {
        assert_eq!(
            eval("[1, 2, 3, 4].filter(value > 2)"),
            serde_json::json!([3, 4])
        );
        assert_eq!(
            eval("[1, 2, 3, 4].filter((x) => x > 2)"),
            serde_json::json!([3, 4])
        );
    }

    #[test]
    fn array_map() {
        assert_eq!(
            eval("[1, 2, 3].map(value + 1)"),
            serde_json::json!([2, 3, 4])
        );
        assert_eq!(
            eval("[5, 6].map((value, index) => value + index)"),
            serde_json::json!([5, 7])
        );
    }

    #[test]
    fn array_reduce() {
        assert_eq!(
            eval("[1, 2, 3].reduce(acc + value, 0)"),
            serde_json::json!(6)
        );
        assert_eq!(
            eval("[1, 2, 3].reduce((accum, curr) => accum + curr, 0)"),
            serde_json::json!(6)
        );
    }

    #[test]
    fn array_contains_all_any() {
        assert_eq!(eval("[1, 2, 3].containsAll(2, 3)"), Value::Bool(true));
        assert_eq!(eval("[1, 2, 3].containsAny(3, 4)"), Value::Bool(true));
    }

    // ── Object methods ──────────────────────────────────────────────

    #[test]
    fn object_keys_values() {
        assert_eq!(
            eval(r#"{"a": 1, "b": 2}.keys()"#),
            serde_json::json!(["a", "b"])
        );
        assert_eq!(
            eval(r#"{"a": 1, "b": 2}.values()"#),
            serde_json::json!([1, 2])
        );
    }

    #[test]
    fn object_is_empty() {
        assert_eq!(eval(r"{}.isEmpty()"), Value::Bool(true));
    }

    // ── Universal methods ───────────────────────────────────────────

    #[test]
    fn is_truthy_method() {
        assert_eq!(eval("(1).isTruthy()"), Value::Bool(true));
        assert_eq!(eval("(0).isTruthy()"), Value::Bool(false));
    }

    #[test]
    fn is_type_method() {
        assert_eq!(eval(r#""hello".isType("string")"#), Value::Bool(true));
        assert_eq!(eval(r#"(42).isType("number")"#), Value::Bool(true));
        assert_eq!(eval(r#"true.isType("boolean")"#), Value::Bool(true));
    }

    #[test]
    fn to_string_method() {
        assert_eq!(eval("(123).toString()"), Value::String("123".to_string()));
    }

    // ── Global functions ────────────────────────────────────────────

    #[test]
    fn if_function() {
        assert_eq!(
            eval(r#"if(true, "yes", "no")"#),
            Value::String("yes".to_string())
        );
        assert_eq!(
            eval(r#"if(false, "yes", "no")"#),
            Value::String("no".to_string())
        );
        assert_eq!(eval("if(false, 1)"), Value::Null);
    }

    #[test]
    fn max_min_functions() {
        assert_eq!(eval("max(1, 5, 3)"), serde_json::json!(5));
        assert_eq!(eval("min(1, 5, 3)"), serde_json::json!(1));
        assert_eq!(eval("max([1, 5, 3])"), serde_json::json!(5));
        assert_eq!(eval("min([1, 5, 3])"), serde_json::json!(1));
        assert_eq!(eval(r#"min("a", "ab", "abc")"#), serde_json::json!("a"));
        assert_eq!(eval(r#"max("a", "ab", "abc")"#), serde_json::json!("abc"));
        assert_eq!(eval("min(null, 6, 2)"), serde_json::json!(2));
        assert_eq!(eval("max(null, 6, 2)"), serde_json::json!(6));
    }

    #[test]
    fn numeric_rounding_functions() {
        assert_eq!(eval("round(16.555555)"), serde_json::json!(17));
        assert_eq!(eval("round(16.555555, 2)"), serde_json::json!(16.56));
        assert_eq!(eval("trunc(12.937)"), serde_json::json!(12));
        assert_eq!(eval("trunc(-0.837764)"), serde_json::json!(0));
        assert_eq!(eval("floor(-93.33333)"), serde_json::json!(-94));
        assert_eq!(eval("ceil(-93.33333)"), serde_json::json!(-93));
    }

    #[test]
    fn aggregate_functions() {
        assert_eq!(eval("sum([2, 3, 1])"), serde_json::json!(6));
        assert_eq!(eval(r#"sum(["a", "b", "c"])"#), serde_json::json!("abc"));
        assert_eq!(eval("sum([])"), Value::Null);
        assert_eq!(eval("product([1, 2, 3])"), serde_json::json!(6));
        assert_eq!(eval("product([])"), Value::Null);
        assert_eq!(eval("average([2, 3, 1])"), serde_json::json!(2));
        assert_eq!(
            eval("average(nonnull([2, 3, null, 1]))"),
            serde_json::json!(2)
        );
        assert_eq!(eval("average([])"), Value::Null);
        assert_eq!(eval(r#"reduce([100, 20, 3], "-")"#), serde_json::json!(77));
        assert_eq!(eval(r#"reduce([200, 10, 2], "/")"#), serde_json::json!(10));
        assert_eq!(
            eval(r#"reduce(["ab", 3], "*")"#),
            serde_json::json!("ababab")
        );
        assert_eq!(
            eval(r#"reduce([true, false, true], "&")"#),
            serde_json::json!(false)
        );
        assert_eq!(
            eval(r"reduce([1, 2, 3], (accum, curr) => accum + curr)"),
            serde_json::json!(6)
        );
    }

    #[test]
    fn minby_maxby_functions() {
        assert_eq!(eval("minby([], (k) => k)"), Value::Null);
        assert_eq!(eval("maxby([], (k) => k)"), Value::Null);
        assert_eq!(eval("minby([1], (k) => k)"), serde_json::json!(1));
        assert_eq!(eval("maxby([1], (k) => k)"), serde_json::json!(1));
        assert_eq!(eval("minby([1, 2, 3], (k) => 0 - k)"), serde_json::json!(3));
        assert_eq!(eval("maxby([1, 2, 3], (k) => 0 - k)"), serde_json::json!(1));
        assert_eq!(eval("minby([1, 2], (k) => null)"), serde_json::json!(1));
    }

    #[test]
    #[allow(clippy::approx_constant)]
    fn number_function() {
        assert_eq!(eval(r#"number("3.14")"#), serde_json::json!(3.14));
    }

    #[test]
    fn list_function() {
        assert_eq!(eval(r#"list("value")"#), serde_json::json!(["value"]));
        assert_eq!(eval("list([1, 2])"), serde_json::json!([1, 2]));
        assert_eq!(eval("list(1, 2, 3)"), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn object_length_and_default_functions() {
        assert_eq!(
            eval(r#"object("a", 1, "b", 2)"#),
            serde_json::json!({"a": 1, "b": 2})
        );
        assert_eq!(eval(r#"length("hello")"#), serde_json::json!(5));
        assert_eq!(eval("length([1, 2, 3])"), serde_json::json!(3));
        assert_eq!(
            eval(r#"length(object("a", 1, "b", 2))"#),
            serde_json::json!(2)
        );
        assert_eq!(eval("length(null)"), serde_json::json!(0));
        assert_eq!(eval("default(null, 1)"), serde_json::json!(1));
        assert_eq!(eval("default(2, 1)"), serde_json::json!(2));
        assert_eq!(
            eval("default(list(1, null, null), 2)"),
            serde_json::json!([1, 2, 2])
        );
        assert_eq!(
            eval("ldefault(list(1, null, null), 2)"),
            serde_json::json!([1, null, null])
        );
        assert_eq!(eval("choice(true, 1, 2)"), serde_json::json!(1));
        assert_eq!(eval("choice(false, 1, 2)"), serde_json::json!(2));
    }

    #[test]
    fn nonnull_firstvalue_and_global_array_functions() {
        assert_eq!(
            eval("nonnull([null, false, 1])"),
            serde_json::json!([false, 1])
        );
        assert_eq!(eval("firstvalue([null, null, 2])"), serde_json::json!(2));
        assert_eq!(
            eval(r#"contains(object("hello", 1), "hello")"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"contains(list("hello"), "he")"#),
            serde_json::json!(true)
        );
        assert_eq!(eval(r#"contains("Hello", "Lo")"#), serde_json::json!(false));
        assert_eq!(eval(r#"contains("Hello", "lo")"#), serde_json::json!(true));
        assert_eq!(eval(r#"icontains("Hello", "Lo")"#), serde_json::json!(true));
        assert_eq!(
            eval(r#"econtains(list("hello", 19), "he")"#),
            serde_json::json!(false)
        );
        assert_eq!(
            eval(r#"econtains(list("hello", 19), 19)"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"containsword("Hello there, chap!", "hello")"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"containsword(list("word", "Words"), "Word")"#),
            serde_json::json!([true, false])
        );
        assert_eq!(eval("all([1, 2, 3])"), serde_json::json!(true));
        assert_eq!(
            eval("all([1, 2, 3], (x) => x > 1)"),
            serde_json::json!(false)
        );
        assert_eq!(eval("all(true, [false])"), serde_json::json!(true));
        assert_eq!(eval("any(true, false)"), serde_json::json!(true));
        assert_eq!(
            eval("any(list(1, 2, 3), (x) => x > 2)"),
            serde_json::json!(true)
        );
        assert_eq!(eval("none([])"), serde_json::json!(true));
        assert_eq!(
            eval("none([1, 2, 3], (x) => x == 0)"),
            serde_json::json!(true)
        );
        assert_eq!(eval("join(list(1, 2, 3))"), serde_json::json!("1, 2, 3"));
        assert_eq!(
            eval(r#"join(list(1, 2, 3), " ")"#),
            serde_json::json!("1 2 3")
        );
        assert_eq!(eval("join(6)"), serde_json::json!("6"));
        assert_eq!(eval("join(list())"), serde_json::json!(""));
        assert_eq!(
            eval("map([1, 2, 3], (x) => x + 2)"),
            serde_json::json!([3, 4, 5])
        );
        assert_eq!(
            eval("filter([1, 2, 3], (x) => x >= 2)"),
            serde_json::json!([2, 3])
        );
        assert_eq!(eval("sort([3, 1, 2])"), serde_json::json!([1, 2, 3]));
        assert_eq!(
            eval("sort([1, 2, 3], (k) => 0 - k)"),
            serde_json::json!([3, 2, 1])
        );
        assert_eq!(eval("reverse([1, 2, 3])"), serde_json::json!([3, 2, 1]));
        assert_eq!(eval("unique([1, 2, 1, 3])"), serde_json::json!([1, 2, 3]));
        assert_eq!(eval("flat([1, [2], [3]])"), serde_json::json!([1, 2, 3]));
        assert_eq!(eval("flat([1, [2, [3]]], 2)"), serde_json::json!([1, 2, 3]));
        assert_eq!(
            eval("slice([1, 2, 3, 4, 5], -2)"),
            serde_json::json!([4, 5])
        );
    }

    #[test]
    fn global_string_functions() {
        assert_eq!(eval(r#"lower("Test")"#), serde_json::json!("test"));
        assert_eq!(
            eval(r#"lower(["YES", "nO"])"#),
            serde_json::json!(["yes", "no"])
        );
        assert_eq!(eval(r#"upper("test")"#), serde_json::json!("TEST"));
        assert_eq!(eval(r#"startswith("yes", "ye")"#), serde_json::json!(true));
        assert_eq!(eval(r#"endswith("yes", "es")"#), serde_json::json!(true));
        assert_eq!(eval(r#"substring("hello", 2, 4)"#), serde_json::json!("ll"));
        assert_eq!(
            eval(r#"split("hello world", " ")"#),
            serde_json::json!(["hello", "world"])
        );
        assert_eq!(
            eval(r#"split("hello there world", "(t?here)")"#),
            serde_json::json!(["hello ", "there", " world"])
        );
        assert_eq!(
            eval(r#"split("hello world", " ", 0)"#),
            serde_json::json!([])
        );
        assert_eq!(
            eval(r#"replace("The big dog chased the big cat.", "big", "small")"#),
            serde_json::json!("The small dog chased the small cat.")
        );
        assert_eq!(
            eval(r#"regextest("what", "what's up dog?")"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"regexmatch("what", "what's up dog?")"#),
            serde_json::json!(false)
        );
        assert_eq!(
            eval(r#"regexreplace("Suite 1000", "\d+", "-")"#),
            serde_json::json!("Suite -")
        );
        assert_eq!(
            eval(r#"truncate("Hello there!", 8)"#),
            serde_json::json!("Hello...")
        );
        assert_eq!(
            eval(r#"truncate("Hello there!", 8, "/")"#),
            serde_json::json!("Hello t/")
        );
        assert_eq!(
            eval(r#"padleft("yes", 5, "!")"#),
            serde_json::json!("!!yes")
        );
        assert_eq!(
            eval(r#"padright("yes", 5, "!")"#),
            serde_json::json!("yes!!")
        );
    }

    #[test]
    fn constructor_and_object_functions() {
        assert_eq!(eval(r#"number("18 years")"#), serde_json::json!(18));
        assert_eq!(eval(r"string(18)"), serde_json::json!("18"));
        assert_eq!(
            eval(r#"dateformat(date("12/31/2022", "MM/dd/yyyy"), "yyyy-MM-dd")"#),
            serde_json::json!("2022-12-31")
        );
        assert_eq!(
            eval(r#"dateformat(date("210313", "yyMMdd"), "yyyy-MM-dd")"#),
            serde_json::json!("2021-03-13")
        );
        assert_eq!(
            eval(r#"dateformat(date([[2021-04-16]]), "yyyy-MM-dd")"#),
            serde_json::json!("2021-04-16")
        );
        assert_eq!(
            eval(r#"string(date("2021-08-15"))"#),
            serde_json::json!("August 15, 2021")
        );
        assert_eq!(
            eval(r#"durationformat(dur(dur("8 hours")), "hh'h'")"#),
            serde_json::json!("08h")
        );
        assert_eq!(
            eval(r#"embed(link("Hello"))"#),
            serde_json::json!("![[Hello]]")
        );
        assert_eq!(
            eval(r#"embed(link("Hello"), false)"#),
            serde_json::json!("[[Hello]]")
        );
        assert_eq!(
            eval(r#"elink("https://example.com", "Example")"#),
            serde_json::json!("[Example](https://example.com)")
        );
        assert_eq!(
            eval(r#"extract(object("a", 1, "b", 2), "b", "c")"#),
            serde_json::json!({"b": 2, "c": null})
        );
        assert_eq!(
            eval(r#"extract([object("a", 1), object("a", 2)], "a")"#),
            serde_json::json!([{"a": 1}, {"a": 2}])
        );
    }

    #[test]
    fn date_duration_and_utility_functions() {
        assert_eq!(
            eval(r#"dateformat(date("2022-01-05T12:18:04"), "yyyy-MM-dd HH:mm:ss")"#),
            serde_json::json!("2022-01-05 12:18:04")
        );
        assert_eq!(
            eval(r#"dateformat(striptime(date("2022-01-05T12:18:04")), "yyyy-MM-dd HH:mm:ss")"#),
            serde_json::json!("2022-01-05 00:00:00")
        );
        assert_eq!(
            eval(r#"dateformat(localtime(date("2022-01-05")), "yyyy-MM-dd")"#),
            serde_json::json!("2022-01-05")
        );
        assert_eq!(
            eval(r#"durationformat(dur("3 days 7 hours 43 seconds"), "ddd'd' hh'h' ss's'")"#),
            serde_json::json!("003d 07h 43s")
        );
        assert_eq!(
            eval(r#"durationformat(dur("14d"), "s 'seconds'")"#),
            serde_json::json!("1209600 seconds")
        );
        assert_eq!(
            eval(r#"display("**Hello** World")"#),
            serde_json::json!("Hello World")
        );
        assert_eq!(
            eval(r#"display("[Hello](https://example.com) [[World]]")"#),
            serde_json::json!("Hello World")
        );
        assert_eq!(
            eval(r#"display(link("path/to/file.md"))"#),
            serde_json::json!("file")
        );
        assert_eq!(
            eval(r#"display(link("path/to/file.md", "displayname"))"#),
            serde_json::json!("displayname")
        );
        assert_eq!(
            eval(r#"display(list("Hello", "World"))"#),
            serde_json::json!("Hello, World")
        );
        assert_eq!(
            eval(r#"currencyformat(123456.789, "EUR")"#),
            serde_json::json!("EUR 123,456.79")
        );
        assert_eq!(
            eval(r#"hash("seed", "text") == hash("seed", "text")"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"hash("seed", "text") != hash("seed", "other")"#),
            serde_json::json!(true)
        );
    }

    #[test]
    fn function_vectorization() {
        assert_eq!(
            eval(r#"replace(list("yes", "re"), "e", "a")"#),
            serde_json::json!(["yas", "ra"])
        );
        assert_eq!(
            eval(r#"replace(["a", "b", "c"], ["a", "b", "c"], "d")"#),
            serde_json::json!(["d", "d", "d"])
        );
        assert_eq!(
            eval(r#"replace(["a", "b", "c"], "a", ["d", "e", "f"])"#),
            serde_json::json!(["d", "b", "c"])
        );
        assert_eq!(
            eval(r#"replace(["a", "b", "c"], ["a", "b", "c"], ["x", "y", "z"])"#),
            serde_json::json!(["x", "y", "z"])
        );
        assert_eq!(
            eval(r#"replace(["a", "b", "c"], ["a", "b"], ["x", "y", "z"])"#),
            serde_json::json!(["x", "y"])
        );
        assert_eq!(
            eval(r#"number(["18 years", "2 months", "hmm"])"#),
            serde_json::json!([18, 2, null])
        );
        assert_eq!(
            eval(r#"dateformat([date("2022-01-05"), date("2022-01-06")], "yyyy-MM-dd")"#),
            serde_json::json!(["2022-01-05", "2022-01-06"])
        );
        assert_eq!(
            eval(r#"substring(["hello", "world"], [1, 2])"#),
            serde_json::json!(["ello", "rld"])
        );
        assert_eq!(
            eval(r#"truncate(["Hello There!", "General Kenobi!"], [8, 10])"#),
            serde_json::json!(["Hello...", "General..."])
        );
        assert_eq!(
            eval(r#"padleft(["a", "bb"], [2, 3], "!")"#),
            serde_json::json!(["!a", "!bb"])
        );
        assert_eq!(
            eval(r#"choice([true, false, true], "left", "right")"#),
            serde_json::json!(["left", "right", "left"])
        );
        assert_eq!(
            eval(r#"link(["A", "B"], ["Alpha", "Beta"])"#),
            serde_json::json!(["[[A|Alpha]]", "[[B|Beta]]"])
        );
        assert_eq!(
            eval(r#"embed(link(["A", "B"]), [true, false])"#),
            serde_json::json!(["![[A]]", "[[B]]"])
        );
        assert_eq!(
            eval(r#"elink(["https://example.com/a", "https://example.com/b"], ["A", "B"])"#),
            serde_json::json!(["[A](https://example.com/a)", "[B](https://example.com/b)"])
        );
        assert_eq!(
            eval(r#"all(regexmatch("a+", list("a", "aaaa")))"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"all(regexmatch("a+", list("a", "aaab")))"#),
            serde_json::json!(false)
        );
        assert_eq!(
            eval(r#"any(regexmatch("a+", list("a", "aaab")))"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"all(regextest("a+", list("a", "aaaa")))"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"all(regextest("a+", list("a", "aaab")))"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"any(regextest("a+", list("a", "aaab")))"#),
            serde_json::json!(true)
        );
        assert_eq!(
            eval(r#"regexreplace(["Suite 1000", "Room 42"], "(\d+)", "<$1>")"#),
            serde_json::json!(["Suite <1000>", "Room <42>"])
        );
    }

    #[test]
    fn escape_html_function() {
        assert_eq!(
            eval(r#"escapeHTML("<b>hi</b>")"#),
            Value::String("&lt;b&gt;hi&lt;/b&gt;".to_string())
        );
    }
}
