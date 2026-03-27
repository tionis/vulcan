use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::expression::ast::{BinOp, Expr, UnOp};
use crate::expression::functions::call_function;
use crate::expression::functions::{date_field_value, parse_date_like_string};
use crate::expression::methods::call_method;
use crate::file_metadata::FileMetadataResolver;
use crate::properties::NoteRecord;

/// Context for evaluating expressions against a single note row.
pub struct EvalContext<'a> {
    pub note: &'a NoteRecord,
    pub formulas: &'a BTreeMap<String, Value>,
    pub now_ms: i64,
    /// Scoped variables for list.filter/map/reduce callbacks.
    pub locals: BTreeMap<String, Value>,
    /// Vault-wide note index keyed by `file_name` (basename without extension).
    /// Used to resolve `.asFile()` and `.linksTo()` on link values.
    pub note_lookup: Option<&'a HashMap<String, NoteRecord>>,
}

impl<'a> EvalContext<'a> {
    #[must_use]
    pub fn new(note: &'a NoteRecord, formulas: &'a BTreeMap<String, Value>) -> Self {
        Self {
            note,
            formulas,
            now_ms: now_millis(),
            locals: BTreeMap::new(),
            note_lookup: None,
        }
    }

    #[must_use]
    pub fn with_note_lookup(mut self, lookup: &'a HashMap<String, NoteRecord>) -> Self {
        self.note_lookup = Some(lookup);
        self
    }
}

pub fn evaluate(expr: &Expr, ctx: &EvalContext) -> Result<Value, String> {
    match expr {
        Expr::Null => Ok(Value::Null),
        Expr::Bool(b) => Ok(Value::Bool(*b)),
        Expr::Number(n) => Ok(number_to_value(*n)),
        Expr::Str(s) => Ok(Value::String(s.clone())),

        Expr::Array(elements) => {
            let values: Result<Vec<Value>, String> =
                elements.iter().map(|e| evaluate(e, ctx)).collect();
            Ok(Value::Array(values?))
        }

        Expr::Object(entries) => {
            let mut map = serde_json::Map::new();
            for (key, value_expr) in entries {
                map.insert(key.clone(), evaluate(value_expr, ctx)?);
            }
            Ok(Value::Object(map))
        }

        Expr::Regex { pattern, flags } => {
            // Store as a structured string for now; methods handle dispatch
            Ok(Value::String(format!("/{pattern}/{flags}")))
        }

        Expr::Identifier(name) => {
            // Check locals first (for list.filter/map/reduce)
            if let Some(value) = ctx.locals.get(name) {
                return Ok(value.clone());
            }
            // `file` standalone → basename string (usable as link target)
            if name == "file" {
                return Ok(Value::String(ctx.note.file_name.clone()));
            }
            // `note` standalone → properties object (so note.title accesses frontmatter)
            if name == "note" {
                return Ok(ctx.note.properties.clone());
            }
            // Then check note properties
            Ok(resolve_property(ctx, name))
        }

        Expr::FieldAccess(receiver, field) => eval_field_access(receiver, field, ctx),

        Expr::FormulaRef(name) => Ok(ctx.formulas.get(name).cloned().unwrap_or(Value::Null)),

        Expr::BinaryOp(left, op, right) => {
            let lval = evaluate(left, ctx)?;
            // Short-circuit for && and ||
            match op {
                BinOp::And => {
                    if !is_truthy(&lval) {
                        return Ok(lval);
                    }
                    return evaluate(right, ctx);
                }
                BinOp::Or => {
                    if is_truthy(&lval) {
                        return Ok(lval);
                    }
                    return evaluate(right, ctx);
                }
                _ => {}
            }
            let rval = evaluate(right, ctx)?;
            Ok(eval_binary_op(&lval, *op, &rval))
        }

        Expr::UnaryOp(op, operand) => {
            let val = evaluate(operand, ctx)?;
            match op {
                UnOp::Not => Ok(Value::Bool(!is_truthy(&val))),
                UnOp::Neg => match &val {
                    Value::Number(n) => {
                        let f = n.as_f64().unwrap_or(0.0);
                        Ok(number_to_value(-f))
                    }
                    _ => Ok(Value::Null),
                },
            }
        }

        Expr::FunctionCall(name, args) => call_function(name, args, ctx),

        Expr::MethodCall(receiver, method, args) => {
            // Special-case: file.method(...) routes to file-specific methods
            if let Expr::Identifier(name) = receiver.as_ref() {
                if name == "file" {
                    return call_file_method(method, args, ctx);
                }
            }
            let receiver_val = evaluate(receiver, ctx)?;
            call_method(&receiver_val, method, args, ctx)
        }
    }
}

fn eval_field_access(receiver: &Expr, field: &str, ctx: &EvalContext) -> Result<Value, String> {
    // Special-case: `file.X` on the implicit file object
    if let Expr::Identifier(name) = receiver {
        if name == "file" {
            return Ok(resolve_file_field(ctx, field));
        }
    }

    let receiver_val = evaluate(receiver, ctx)?;

    // Field access on different types
    match &receiver_val {
        // String fields
        Value::String(s) => {
            if field == "length" {
                return Ok(Value::Number(s.chars().count().into()));
            }
            if let Some(ms) = parse_date_like_string(s) {
                if let Some(value) = date_field_value(ms, field) {
                    return Ok(value);
                }
            }
            Ok(Value::Null)
        }

        // Array fields
        Value::Array(arr) => match field {
            "length" => Ok(Value::Number(arr.len().into())),
            _ => Ok(Value::Null),
        },

        // Object field access
        Value::Object(map) => Ok(map.get(field).cloned().unwrap_or(Value::Null)),

        Value::Number(n) => {
            if let Some(ms) = n.as_i64() {
                if let Some(value) = date_field_value(ms, field) {
                    return Ok(value);
                }
            }
            Ok(Value::Null)
        }

        _ => Ok(Value::Null),
    }
}

fn resolve_file_field(ctx: &EvalContext, field: &str) -> Value {
    FileMetadataResolver::field(ctx.note, field)
}

/// Convert a `NoteRecord` into the file object Value returned by `.asFile()`.
#[must_use]
pub fn note_to_file_object(note: &NoteRecord) -> Value {
    FileMetadataResolver::object(note)
}

/// Parse the target filename from a wikilink string like `[[target]]` or `[[target|display]]`.
/// Falls back to the raw string if it is not a wikilink (treating it as a plain filename).
#[must_use]
pub fn parse_wikilink_target(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with("[[") && s.ends_with("]]") {
        let inner = &s[2..s.len() - 2];
        // Strip display text
        let inner = inner.find('|').map_or(inner, |i| &inner[..i]);
        // Strip heading/block anchor
        let inner = inner.find('#').map_or(inner, |i| &inner[..i]);
        inner.trim()
    } else {
        s
    }
}

fn call_file_method(
    method: &str,
    args: &[crate::expression::ast::Expr],
    ctx: &EvalContext,
) -> Result<Value, String> {
    use crate::expression::methods::eval_arg;
    match method {
        "hasTag" => {
            let tag_arg = eval_arg(args, 0, ctx)?;
            let tag = match &tag_arg {
                Value::String(s) => s.trim_start_matches('#').to_string(),
                _ => return Ok(Value::Bool(false)),
            };
            let matches = ctx.note.tags.iter().any(|t| {
                let t = t.trim_start_matches('#');
                t == tag || t.starts_with(&format!("{tag}/"))
            });
            Ok(Value::Bool(matches))
        }
        "hasProperty" => {
            let prop_arg = eval_arg(args, 0, ctx)?;
            let prop = match &prop_arg {
                Value::String(s) => s.as_str(),
                _ => return Ok(Value::Bool(false)),
            };
            let has = ctx
                .note
                .properties
                .as_object()
                .is_some_and(|m| m.contains_key(prop) && m.get(prop) != Some(&Value::Null));
            Ok(Value::Bool(has))
        }
        "inFolder" => {
            let folder_arg = eval_arg(args, 0, ctx)?;
            let folder = match &folder_arg {
                Value::String(s) => s.trim_end_matches('/').to_string(),
                _ => return Ok(Value::Bool(false)),
            };
            let in_folder = ctx.note.document_path.starts_with(&format!("{folder}/"));
            Ok(Value::Bool(in_folder))
        }
        "hasLink" => {
            let link_arg = eval_arg(args, 0, ctx)?;
            let target = match &link_arg {
                Value::String(s) => s.as_str(),
                _ => return Ok(Value::Bool(false)),
            };
            let has = ctx.note.links.iter().any(|l| l == target);
            Ok(Value::Bool(has))
        }
        "asLink" => {
            let path = &ctx.note.document_path;
            let basename = path.rfind('/').map_or(path.as_str(), |i| &path[i + 1..]);
            let basename = basename.trim_end_matches(".md");
            Ok(Value::String(format!("[[{basename}]]")))
        }
        _ => Err(format!("unknown file method `{method}`")),
    }
}

fn resolve_property(ctx: &EvalContext, name: &str) -> Value {
    if let Some(props) = ctx.note.properties.as_object() {
        props.get(name).cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    }
}

fn eval_binary_op(left: &Value, op: BinOp, right: &Value) -> Value {
    match op {
        BinOp::Add => eval_add(left, right),
        BinOp::Sub => eval_sub(left, right),
        BinOp::Mul => eval_mul(left, right),
        BinOp::Div => {
            if left.is_null() || right.is_null() {
                return Value::Null;
            }
            let b = as_number(right);
            if b == 0.0 {
                return Value::Null;
            }
            eval_div(left, right)
        }
        BinOp::Mod => eval_mod(left, right),
        BinOp::Eq => Value::Bool(values_equal(left, right)),
        BinOp::Ne => Value::Bool(!values_equal(left, right)),
        BinOp::Gt => Value::Bool(compare_values(left, right) == Some(std::cmp::Ordering::Greater)),
        BinOp::Lt => Value::Bool(compare_values(left, right) == Some(std::cmp::Ordering::Less)),
        BinOp::Ge => Value::Bool(matches!(
            compare_values(left, right),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        )),
        BinOp::Le => Value::Bool(matches!(
            compare_values(left, right),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        )),
        BinOp::And | BinOp::Or => unreachable!("handled by short-circuit"),
    }
}

#[allow(clippy::cast_precision_loss)]
fn eval_add(left: &Value, right: &Value) -> Value {
    if left.is_null() || right.is_null() {
        return Value::Null;
    }

    if let Some(left_date_ms) = date_string_value_ms(left) {
        if let Some(right_duration_ms) = duration_like_value_ms(right) {
            return number_to_value((left_date_ms + right_duration_ms) as f64);
        }
    }
    if let Some(right_date_ms) = date_string_value_ms(right) {
        if let Some(left_duration_ms) = duration_like_value_ms(left) {
            return number_to_value((left_duration_ms + right_date_ms) as f64);
        }
    }
    if let Some(left_duration_ms) = duration_string_value_ms(left) {
        if let Some(right_duration_ms) = duration_like_value_ms(right) {
            return number_to_value((left_duration_ms + right_duration_ms) as f64);
        }
    }
    if let Some(right_duration_ms) = duration_string_value_ms(right) {
        if let Some(left_duration_ms) = duration_like_value_ms(left) {
            return number_to_value((left_duration_ms + right_duration_ms) as f64);
        }
    }

    // String concatenation if either side is a string
    match (left, right) {
        (Value::String(a), Value::String(b)) => Value::String(format!("{a}{b}")),
        (Value::String(a), _) => Value::String(format!("{a}{}", value_to_display(right))),
        (_, Value::String(b)) => Value::String(format!("{}{b}", value_to_display(left))),
        _ => eval_arithmetic(left, right, |a, b| a + b),
    }
}

#[allow(clippy::cast_precision_loss)]
fn eval_sub(left: &Value, right: &Value) -> Value {
    if left.is_null() || right.is_null() {
        return Value::Null;
    }

    if let Some(left_date_ms) = date_string_value_ms(left) {
        if let Some(right_date_ms) = date_like_value_ms(right) {
            return number_to_value((left_date_ms - right_date_ms) as f64);
        }
        if let Some(right_duration_ms) = duration_like_value_ms(right) {
            return number_to_value((left_date_ms - right_duration_ms) as f64);
        }
    }
    if let Some(right_date_ms) = date_string_value_ms(right) {
        if let Some(left_date_ms) = integer_value_ms(left) {
            return number_to_value((left_date_ms - right_date_ms) as f64);
        }
    }
    if let Some(left_duration_ms) = duration_string_value_ms(left) {
        if let Some(right_duration_ms) = duration_like_value_ms(right) {
            return number_to_value((left_duration_ms - right_duration_ms) as f64);
        }
    }
    if let Some(right_duration_ms) = duration_string_value_ms(right) {
        if let Some(left_duration_ms) = duration_like_value_ms(left) {
            return number_to_value((left_duration_ms - right_duration_ms) as f64);
        }
    }
    eval_arithmetic(left, right, |a, b| a - b)
}

#[allow(clippy::cast_precision_loss)]
fn eval_mul(left: &Value, right: &Value) -> Value {
    if left.is_null() || right.is_null() {
        return Value::Null;
    }

    if let Some(duration_ms) = duration_string_value_ms(left) {
        let factor = as_number(right);
        if factor.is_finite() {
            return number_to_value(duration_ms as f64 * factor);
        }
    }
    if let Some(duration_ms) = duration_string_value_ms(right) {
        let factor = as_number(left);
        if factor.is_finite() {
            return number_to_value(duration_ms as f64 * factor);
        }
    }

    if let Some(repeated) = repeat_string(left, right).or_else(|| repeat_string(right, left)) {
        return Value::String(repeated);
    }

    eval_arithmetic(left, right, |a, b| a * b)
}

#[allow(clippy::cast_precision_loss)]
fn eval_div(left: &Value, right: &Value) -> Value {
    if let Some(left_duration_ms) = duration_string_value_ms(left) {
        let divisor = as_number(right);
        if divisor.is_finite() && divisor != 0.0 {
            return number_to_value(left_duration_ms as f64 / divisor);
        }
    }
    eval_arithmetic(left, right, |a, b| a / b)
}

#[allow(clippy::cast_precision_loss)]
fn eval_mod(left: &Value, right: &Value) -> Value {
    if left.is_null() || right.is_null() {
        return Value::Null;
    }
    let divisor = as_number(right);
    if divisor == 0.0 {
        return Value::Null;
    }
    if let Some(left_duration_ms) = duration_string_value_ms(left) {
        if divisor.is_finite() {
            return number_to_value(left_duration_ms as f64 % divisor);
        }
    }
    eval_arithmetic(left, right, |a, b| a % b)
}

fn eval_arithmetic(left: &Value, right: &Value, f: fn(f64, f64) -> f64) -> Value {
    if left.is_null() || right.is_null() {
        return Value::Null;
    }
    let a = as_number(left);
    let b = as_number(right);
    if a.is_nan() || b.is_nan() {
        return Value::Null;
    }
    number_to_value(f(a, b))
}

fn values_equal(left: &Value, right: &Value) -> bool {
    if let Some(ordering) = compare_date_like_values(left, right) {
        return ordering == std::cmp::Ordering::Equal;
    }
    if let Some(ordering) = compare_duration_like_values(left, right) {
        return ordering == std::cmp::Ordering::Equal;
    }
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => a == b,
        _ => false,
    }
}

pub(crate) fn compare_values(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (Value::Null, Value::Null) => return Some(std::cmp::Ordering::Equal),
        (Value::Null, _) => return Some(std::cmp::Ordering::Less),
        (_, Value::Null) => return Some(std::cmp::Ordering::Greater),
        _ => {}
    }
    if let Some(ordering) = compare_date_like_values(left, right) {
        return Some(ordering);
    }
    if let Some(ordering) = compare_duration_like_values(left, right) {
        return Some(ordering);
    }
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&b.as_f64().unwrap_or(0.0)),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

fn compare_date_like_values(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (date_string_value_ms(left), date_string_value_ms(right)) {
        (Some(left_ms), Some(right_ms)) => Some(left_ms.cmp(&right_ms)),
        (Some(left_ms), None) => integer_value_ms(right).map(|right_ms| left_ms.cmp(&right_ms)),
        (None, Some(right_ms)) => integer_value_ms(left).map(|left_ms| left_ms.cmp(&right_ms)),
        (None, None) => None,
    }
}

fn compare_duration_like_values(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (
        duration_string_value_ms(left),
        duration_string_value_ms(right),
    ) {
        (Some(left_ms), Some(right_ms)) => Some(left_ms.cmp(&right_ms)),
        (Some(left_ms), None) => integer_value_ms(right).map(|right_ms| left_ms.cmp(&right_ms)),
        (None, Some(right_ms)) => integer_value_ms(left).map(|left_ms| left_ms.cmp(&right_ms)),
        (None, None) => None,
    }
}

fn date_like_value_ms(value: &Value) -> Option<i64> {
    date_string_value_ms(value).or_else(|| integer_value_ms(value))
}

fn date_string_value_ms(value: &Value) -> Option<i64> {
    let Value::String(text) = value else {
        return None;
    };
    parse_date_like_string(text)
}

fn duration_like_value_ms(value: &Value) -> Option<i64> {
    duration_string_value_ms(value).or_else(|| integer_value_ms(value))
}

fn duration_string_value_ms(value: &Value) -> Option<i64> {
    let Value::String(text) = value else {
        return None;
    };
    crate::expression::functions::parse_duration_string(text)
}

fn integer_value_ms(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            let value = number.as_f64()?;
            if value.is_finite() && value.fract() == 0.0 {
                Some(value as i64)
            } else {
                None
            }
        }),
        _ => None,
    }
}

fn repeat_string(string_value: &Value, count_value: &Value) -> Option<String> {
    let Value::String(text) = string_value else {
        return None;
    };
    if date_string_value_ms(string_value).is_some()
        || duration_string_value_ms(string_value).is_some()
    {
        return None;
    }
    let count = integer_value_ms(count_value)?;
    usize::try_from(count).ok().map(|count| text.repeat(count))
}

#[must_use]
pub fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(m) => !m.is_empty(),
    }
}

#[must_use]
pub fn as_number(value: &Value) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Bool(true) => 1.0,
        Value::Bool(false) => 0.0,
        Value::String(s) => s.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

#[must_use]
pub fn value_to_display(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            // Intentional: check if float is exactly an integer value
            #[allow(clippy::float_cmp)]
            if f == f.trunc() && f.abs() < 1e15 {
                #[allow(clippy::cast_possible_truncation)]
                return format!("{}", f as i64);
            }
            n.to_string()
        }
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

pub fn number_to_value(n: f64) -> Value {
    // Intentional: check if float is exactly an integer value within i64 range
    #[allow(clippy::float_cmp, clippy::cast_precision_loss)]
    if n == n.trunc() && n.abs() < (i64::MAX as f64) {
        #[allow(clippy::cast_possible_truncation)]
        return Value::Number(serde_json::Number::from(n as i64));
    }
    serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number)
}

#[allow(clippy::cast_possible_truncation)]
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::parse::Parser;

    fn eval(input: &str) -> Value {
        eval_with_now(input, 1_776_482_700_000)
    }

    fn eval_with_now(input: &str, now_ms: i64) -> Value {
        let expr = Parser::new(input).unwrap().parse().unwrap();
        let note = NoteRecord {
            document_id: "note-id".to_string(),
            document_path: "folder/note.md".to_string(),
            file_name: "note".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1_700_000_000,
            file_size: 1234,
            properties: serde_json::json!({
                "status": "done",
                "count": 42,
                "tags": ["a", "b"],
                "due": "2026-04",
                "estimate": "1d 3h",
                "author": "[[alice]]"
            }),
            tags: vec!["#tag1".to_string(), "#tag2/nested".to_string()],
            links: vec!["other.md".to_string()],
            inlinks: vec!["[[home]]".to_string()],
            aliases: vec!["Note Alias".to_string()],
            frontmatter: serde_json::json!({"Date": "2026-04-18", "status": "done"}),
            list_items: vec![],
            tasks: vec![],
        };
        let formulas = BTreeMap::new();
        let mut ctx = EvalContext::new(&note, &formulas);
        ctx.now_ms = now_ms;
        evaluate(&expr, &ctx).unwrap()
    }

    #[test]
    fn eval_literals() {
        assert_eq!(eval("null"), Value::Null);
        assert_eq!(eval("true"), Value::Bool(true));
        assert_eq!(eval("42"), serde_json::json!(42));
        assert_eq!(eval(r#""hello""#), Value::String("hello".to_string()));
    }

    #[test]
    fn eval_arithmetic() {
        assert_eq!(eval("1 + 2"), serde_json::json!(3));
        assert_eq!(eval("10 - 3"), serde_json::json!(7));
        assert_eq!(eval("4 * 5"), serde_json::json!(20));
        assert_eq!(eval("10 / 3"), serde_json::json!(10.0 / 3.0));
        assert_eq!(eval("7 % 4"), serde_json::json!(3));
        assert_eq!(eval("(1 + 2) * 3"), serde_json::json!(9));
    }

    #[test]
    fn eval_string_concatenation() {
        assert_eq!(
            eval(r#""hello" + " " + "world""#),
            Value::String("hello world".to_string())
        );
        assert_eq!(
            eval(r#""count: " + 42"#),
            Value::String("count: 42".to_string())
        );
    }

    #[test]
    fn eval_comparison() {
        assert_eq!(eval("1 > 0"), Value::Bool(true));
        assert_eq!(eval("1 < 0"), Value::Bool(false));
        assert_eq!(eval("1 == 1"), Value::Bool(true));
        assert_eq!(eval("1 != 2"), Value::Bool(true));
        assert_eq!(eval("5 >= 5"), Value::Bool(true));
        assert_eq!(eval("5 <= 4"), Value::Bool(false));
    }

    #[test]
    fn eval_boolean_logic() {
        assert_eq!(eval("true && false"), Value::Bool(false));
        assert_eq!(eval("true || false"), Value::Bool(true));
        assert_eq!(eval("!true"), Value::Bool(false));
        assert_eq!(eval("!false"), Value::Bool(true));
        assert_eq!(eval("!null"), Value::Bool(true));
    }

    #[test]
    fn eval_property_access() {
        assert_eq!(eval("status"), Value::String("done".to_string()));
        assert_eq!(eval("count"), serde_json::json!(42));
        assert_eq!(eval("nonexistent"), Value::Null);
    }

    #[test]
    fn eval_file_fields() {
        assert_eq!(
            eval("file.path"),
            Value::String("folder/note.md".to_string())
        );
        assert_eq!(eval("file.name"), Value::String("note".to_string()));
        assert_eq!(eval("file.basename"), Value::String("note".to_string()));
        assert_eq!(eval("file.ext"), Value::String("md".to_string()));
        assert_eq!(eval("file.folder"), Value::String("folder".to_string()));
        assert_eq!(
            eval("file.link"),
            Value::String("[[folder/note]]".to_string())
        );
        assert_eq!(eval("file.size"), serde_json::json!(1234));
        assert_eq!(eval("file.mtime"), serde_json::json!(1_700_000_000));
        assert_eq!(eval("file.mday"), Value::String("1970-01-20".to_string()));
        assert_eq!(eval("file.mtime.year"), serde_json::json!(1970));
        assert_eq!(eval("file.mtime.month"), serde_json::json!(1));
        assert_eq!(eval("file.mtime.weekday"), serde_json::json!(2));
    }

    #[test]
    fn eval_file_tags_and_links() {
        assert_eq!(
            eval("file.tags"),
            serde_json::json!(["#tag1", "#tag2", "#tag2/nested"])
        );
        assert_eq!(
            eval("file.etags"),
            serde_json::json!(["#tag1", "#tag2/nested"])
        );
        assert_eq!(eval("file.links"), serde_json::json!(["other.md"]));
        assert_eq!(eval("file.outlinks"), serde_json::json!(["other.md"]));
        assert_eq!(eval("file.inlinks"), serde_json::json!(["[[home]]"]));
        assert_eq!(eval("file.aliases"), serde_json::json!(["Note Alias"]));
    }

    #[test]
    fn eval_file_day_and_frontmatter() {
        assert_eq!(eval("file.day"), Value::String("2026-04-18".to_string()));
        assert_eq!(eval("file.day.year"), serde_json::json!(2026));
        assert_eq!(eval("due.month"), serde_json::json!(4));
        assert_eq!(
            eval("file.frontmatter.status"),
            Value::String("done".to_string())
        );
    }

    #[test]
    fn eval_date_shortcuts() {
        use crate::expression::functions::parse_date_string;

        let now_ms = 1_776_482_700_000;

        assert_eq!(
            eval_with_now("date(now)", now_ms),
            serde_json::json!(now_ms)
        );
        assert_eq!(
            eval_with_now("date(today)", now_ms),
            serde_json::json!(parse_date_string("2026-04-18").unwrap())
        );
        assert_eq!(
            eval_with_now("date(tomorrow)", now_ms),
            serde_json::json!(parse_date_string("2026-04-19").unwrap())
        );
        assert_eq!(
            eval_with_now("date(yesterday)", now_ms),
            serde_json::json!(parse_date_string("2026-04-17").unwrap())
        );
        assert_eq!(
            eval_with_now("date(sow)", now_ms),
            serde_json::json!(parse_date_string("2026-04-13").unwrap())
        );
        assert_eq!(
            eval_with_now("date(eow)", now_ms),
            serde_json::json!(parse_date_string("2026-04-19").unwrap())
        );
        assert_eq!(
            eval_with_now("date(som)", now_ms),
            serde_json::json!(parse_date_string("2026-04-01").unwrap())
        );
        assert_eq!(
            eval_with_now("date(eom)", now_ms),
            serde_json::json!(parse_date_string("2026-04-30").unwrap())
        );
        assert_eq!(
            eval_with_now("date(soy)", now_ms),
            serde_json::json!(parse_date_string("2026-01-01").unwrap())
        );
        assert_eq!(
            eval_with_now("date(eoy)", now_ms),
            serde_json::json!(parse_date_string("2026-12-31").unwrap())
        );
    }

    #[test]
    fn eval_unquoted_date_and_duration_literals() {
        assert_eq!(
            eval("date(2026-04-18)"),
            serde_json::json!(1_776_470_400_000_i64)
        );
        assert_eq!(eval("dur(1d 3h 20m)"), serde_json::json!(98_400_000_i64));
    }

    #[test]
    fn eval_typeof_and_duration_alias() {
        assert_eq!(
            eval("duration(\"1d 3h\")"),
            serde_json::json!(97_200_000_i64)
        );
        assert_eq!(eval("dur(1d 3h)"), serde_json::json!(97_200_000_i64));
        assert_eq!(eval("typeof(due)"), Value::String("date".to_string()));
        assert_eq!(
            eval("typeof(estimate)"),
            Value::String("duration".to_string())
        );
        assert_eq!(eval("typeof(author)"), Value::String("link".to_string()));
        assert_eq!(
            eval("typeof(file.mtime)"),
            Value::String("date".to_string())
        );
        assert_eq!(
            eval("typeof(dur(1d 3h))"),
            Value::String("duration".to_string())
        );
    }

    #[test]
    fn eval_type_coercion_rules() {
        use crate::expression::functions::parse_date_like_string;

        assert_eq!(
            eval("date(2026-04-18) - date(2026-04-17)"),
            serde_json::json!(86_400_000_i64)
        );
        assert_eq!(
            eval("due + dur(1d)"),
            serde_json::json!(parse_date_like_string("2026-04-02").unwrap())
        );
        assert_eq!(
            eval("estimate + dur(1h)"),
            serde_json::json!(100_800_000_i64)
        );
        assert_eq!(eval(r#""ha" * 3"#), Value::String("hahaha".to_string()));
        assert_eq!(eval(r#"3 * "ha""#), Value::String("hahaha".to_string()));
        assert_eq!(eval("estimate * 2"), serde_json::json!(194_400_000_i64));
    }

    #[test]
    fn eval_null_ordering_and_propagation() {
        assert_eq!(eval("null < 0"), Value::Bool(true));
        assert_eq!(eval("null <= date(today)"), Value::Bool(true));
        assert_eq!(eval("nonexistent < 0"), Value::Bool(true));
        assert_eq!(eval("1 + null"), Value::Null);
        assert_eq!(eval("null * 3"), Value::Null);
        assert_eq!(eval("10 % null"), Value::Null);
    }

    #[test]
    fn eval_date_and_duration_comparisons() {
        assert_eq!(eval("due < date(2026-05-01)"), Value::Bool(true));
        assert_eq!(eval("due == date(2026-04)"), Value::Bool(true));
        assert_eq!(eval("estimate == dur(1d 3h)"), Value::Bool(true));
        assert_eq!(eval("estimate > dur(1d)"), Value::Bool(true));
    }

    #[test]
    fn eval_string_length() {
        assert_eq!(eval(r#""hello".length"#), serde_json::json!(5));
    }

    #[test]
    fn eval_array_length() {
        assert_eq!(eval("[1, 2, 3].length"), serde_json::json!(3));
    }

    #[test]
    fn eval_formula_ref() {
        let expr = Parser::new("formula.total").unwrap().parse().unwrap();
        let note = NoteRecord {
            document_id: "test-id".to_string(),
            document_path: "test.md".to_string(),
            file_name: "test".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 0,
            file_size: 0,
            properties: serde_json::json!({}),
            tags: vec![],
            links: vec![],
            inlinks: vec![],
            aliases: vec![],
            frontmatter: serde_json::json!({}),
            list_items: vec![],
            tasks: vec![],
        };
        let mut formulas = BTreeMap::new();
        formulas.insert("total".to_string(), serde_json::json!(100));
        let ctx = EvalContext::new(&note, &formulas);
        assert_eq!(evaluate(&expr, &ctx).unwrap(), serde_json::json!(100));
    }

    #[test]
    fn eval_division_by_zero() {
        assert_eq!(eval("10 / 0"), Value::Null);
    }

    #[test]
    fn eval_negation() {
        assert_eq!(eval("-5"), serde_json::json!(-5));
        assert_eq!(eval("-(3 + 2)"), serde_json::json!(-5));
    }

    #[test]
    fn eval_nested_boolean() {
        assert_eq!(eval("1 > 0 && 2 > 1"), Value::Bool(true));
        assert_eq!(eval("1 > 0 && 2 < 1"), Value::Bool(false));
    }

    #[test]
    fn eval_object_field_access() {
        assert_eq!(
            eval("file.properties.status"),
            Value::String("done".to_string())
        );
    }

    #[test]
    fn eval_array_literal() {
        assert_eq!(eval("[1, 2, 3]"), serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn eval_object_literal() {
        assert_eq!(
            eval(r#"{"a": 1, "b": 2}"#),
            serde_json::json!({"a": 1, "b": 2})
        );
    }

    fn eval_with_lookup(input: &str, lookup: &HashMap<String, NoteRecord>) -> Value {
        let expr = Parser::new(input).unwrap().parse().unwrap();
        let note = NoteRecord {
            document_id: "note-id".to_string(),
            document_path: "folder/note.md".to_string(),
            file_name: "note".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1_700_000_000,
            file_size: 1234,
            properties: serde_json::json!({"author": "[[alice]]"}),
            tags: vec![],
            links: vec!["[[alice]]".to_string()],
            inlinks: vec![],
            aliases: vec![],
            frontmatter: serde_json::json!({}),
            list_items: vec![],
            tasks: vec![],
        };
        let formulas = BTreeMap::new();
        let ctx = EvalContext::new(&note, &formulas).with_note_lookup(lookup);
        evaluate(&expr, &ctx).unwrap()
    }

    #[test]
    fn eval_as_file_and_links_to() {
        let alice = NoteRecord {
            document_id: "alice-id".to_string(),
            document_path: "people/alice.md".to_string(),
            file_name: "alice".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1_700_000_001,
            file_size: 500,
            properties: serde_json::json!({"role": "editor"}),
            tags: vec!["#person".to_string()],
            links: vec!["[[note]]".to_string()],
            inlinks: vec!["[[note]]".to_string()],
            aliases: vec!["Alice".to_string()],
            frontmatter: serde_json::json!({"role": "editor"}),
            list_items: vec![],
            tasks: vec![],
        };
        let mut lookup = HashMap::new();
        lookup.insert("alice".to_string(), alice);

        // asFile() on a wikilink returns the file object
        let file_obj = eval_with_lookup(r#""[[alice]]".asFile()"#, &lookup);
        assert_eq!(
            file_obj.get("name").unwrap(),
            &Value::String("alice".to_string())
        );
        assert_eq!(
            file_obj.get("ext").unwrap(),
            &Value::String("md".to_string())
        );
        assert_eq!(
            file_obj.get("properties").unwrap().get("role").unwrap(),
            &Value::String("editor".to_string())
        );

        // asFile() on unknown link returns null
        assert_eq!(
            eval_with_lookup(r#""[[nobody]]".asFile()"#, &lookup),
            Value::Null
        );

        // linksTo() checks outbound links of the source note
        assert_eq!(
            eval_with_lookup(r#""[[alice]]".linksTo("[[note]]")"#, &lookup),
            Value::Bool(true)
        );
        assert_eq!(
            eval_with_lookup(r#""[[alice]]".linksTo("[[other]]")"#, &lookup),
            Value::Bool(false)
        );

        // asFile().property navigation
        assert_eq!(
            eval_with_lookup(r#""[[alice]]".asFile().properties.role"#, &lookup),
            Value::String("editor".to_string())
        );
    }
}
