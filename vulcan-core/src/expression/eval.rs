use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::expression::ast::{BinOp, Expr, UnOp};
use crate::expression::functions::call_function;
use crate::expression::functions::{date_field_value, parse_date_like_string, parse_wikilink};
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
            let normalized_name = normalize_field_name(name);
            if normalized_name == "this" {
                return Ok(note_to_page_object(ctx.note));
            }
            // `file` standalone → basename string (usable as link target)
            if normalized_name == "file" {
                return Ok(Value::String(ctx.note.file_name.clone()));
            }
            // `note` standalone → properties object (so note.title accesses frontmatter)
            if normalized_name == "note" {
                return Ok(ctx.note.properties.clone());
            }
            // Then check note properties
            Ok(resolve_property(ctx, name))
        }

        Expr::FieldAccess(receiver, field) => eval_field_access(receiver, field, ctx),

        Expr::IndexAccess(receiver, index) => eval_index_access(receiver, index, ctx),

        Expr::Lambda(_, _) => Err("cannot evaluate lambda outside a higher-order call".to_string()),

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
        if normalize_field_name(name) == "file" {
            return Ok(resolve_file_field(ctx, field));
        }
    }

    if let Some(value) = resolve_task_status_metadata_field(receiver, field, ctx)? {
        return Ok(value);
    }

    let receiver_val = evaluate(receiver, ctx)?;
    Ok(resolve_value_field(&receiver_val, field, ctx))
}

fn resolve_task_status_metadata_field(
    receiver: &Expr,
    field: &str,
    ctx: &EvalContext,
) -> Result<Option<Value>, String> {
    let Expr::FieldAccess(base, status_field) = receiver else {
        return Ok(None);
    };
    if normalize_field_name(status_field) != "status" {
        return Ok(None);
    }

    let field_key = match normalize_field_name(field).as_str() {
        "symbol" => "status",
        "name" => "statusName",
        "type" => "statusType",
        "next" | "nextsymbol" | "nextstatussymbol" => "statusNext",
        _ => return Ok(None),
    };

    Ok(Some(match evaluate(base, ctx)? {
        Value::Object(object) => normalized_object_field(&object, field_key)
            .cloned()
            .unwrap_or(Value::Null),
        Value::Array(values) => swizzle_array_field(&values, field_key, ctx),
        _ => Value::Null,
    }))
}

fn eval_index_access(receiver: &Expr, index: &Expr, ctx: &EvalContext) -> Result<Value, String> {
    if let Expr::Identifier(name) = receiver {
        if normalize_field_name(name) == "file" {
            let field = evaluate(index, ctx)?;
            return Ok(match field {
                Value::String(field) => resolve_file_field(ctx, &field),
                _ => Value::Null,
            });
        }
    }

    let receiver_val = evaluate(receiver, ctx)?;
    let index_val = evaluate(index, ctx)?;

    match (&receiver_val, &index_val) {
        (Value::Array(values), Value::String(field)) => Ok(swizzle_array_field(values, field, ctx)),
        (Value::String(link), Value::String(field)) if parse_wikilink(link).is_some() => {
            Ok(resolve_link_field(ctx, link, field))
        }
        (Value::Array(values), _) => {
            let Some(index) =
                integer_value_ms(&index_val).and_then(|index| usize::try_from(index).ok())
            else {
                return Ok(Value::Null);
            };
            Ok(values.get(index).cloned().unwrap_or(Value::Null))
        }
        (Value::Object(object), Value::String(key)) => Ok(normalized_object_field(object, key)
            .cloned()
            .unwrap_or(Value::Null)),
        _ => Ok(Value::Null),
    }
}

fn resolve_file_field(ctx: &EvalContext, field: &str) -> Value {
    let field = canonical_file_field_name(field);
    FileMetadataResolver::field(ctx.note, &field)
}

fn resolve_value_field(value: &Value, field: &str, ctx: &EvalContext) -> Value {
    let normalized_field = normalize_field_name(field);
    match value {
        Value::String(s) => {
            if parse_wikilink(s).is_some() {
                return resolve_link_field(ctx, s, field);
            }
            if normalized_field == "length" {
                return Value::Number(s.chars().count().into());
            }
            if let Some(ms) = parse_date_like_string(s) {
                if let Some(value) = date_field_value(ms, &canonical_date_field_name(field)) {
                    return value;
                }
            }
            Value::Null
        }
        Value::Array(arr) => {
            if normalized_field == "length" {
                Value::Number(arr.len().into())
            } else {
                swizzle_array_field(arr, field, ctx)
            }
        }
        Value::Object(map) => normalized_object_field(map, field)
            .cloned()
            .unwrap_or(Value::Null),
        Value::Number(n) => n
            .as_i64()
            .and_then(|ms| date_field_value(ms, &canonical_date_field_name(field)))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

fn swizzle_array_field(values: &[Value], field: &str, ctx: &EvalContext) -> Value {
    let mut result = Vec::new();
    for value in values {
        match resolve_value_field(value, field, ctx) {
            Value::Array(items) => result.extend(items),
            other => result.push(other),
        }
    }
    Value::Array(result)
}

/// Convert a `NoteRecord` into the file object Value returned by `.asFile()`.
#[must_use]
pub fn note_to_file_object(note: &NoteRecord) -> Value {
    FileMetadataResolver::object(note)
}

#[must_use]
pub fn note_to_page_object(note: &NoteRecord) -> Value {
    let mut object = note
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    object.insert("file".to_string(), note_to_file_object(note));
    Value::Object(object)
}

/// Parse the target filename from a wikilink string like `[[target]]` or `[[target|display]]`.
/// Falls back to the raw string if it is not a wikilink (treating it as a plain filename).
#[must_use]
pub fn parse_wikilink_target(s: &str) -> String {
    parse_wikilink(s).map_or_else(|| s.trim().to_string(), |link| link.path)
}

pub(crate) fn resolve_note_reference<'a>(
    lookup: &'a HashMap<String, NoteRecord>,
    source_path: &str,
    target: &str,
) -> Option<&'a NoteRecord> {
    let target = target.trim();
    let target_no_ext = target.trim_end_matches(".md");
    let target_basename = target_no_ext.rsplit('/').next().unwrap_or(target_no_ext);

    if let Some(note) = lookup.values().find(|note| {
        note.document_path == target || note.document_path.trim_end_matches(".md") == target_no_ext
    }) {
        return Some(note);
    }

    if let Some(note) = lookup.get(target_no_ext) {
        return Some(note);
    }

    let source_folder = source_path
        .rsplit_once('/')
        .map_or("", |(folder, _)| folder);

    lookup
        .values()
        .filter_map(|note| {
            let rank = if note.file_name == target_basename {
                Some(0_usize)
            } else if note
                .aliases
                .iter()
                .any(|alias| alias == target || alias == target_basename)
            {
                Some(1_usize)
            } else {
                None
            }?;
            Some((
                rank,
                folder_distance(source_folder, &note.document_path),
                note.document_path.as_str(),
                note,
            ))
        })
        .min_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.cmp(&right.1))
                .then(left.2.cmp(right.2))
        })
        .map(|(_, _, _, note)| note)
}

fn resolve_link_field(ctx: &EvalContext, link: &str, field: &str) -> Value {
    let Some(link_meta) = parse_wikilink(link) else {
        return Value::Null;
    };
    let Some(lookup) = ctx.note_lookup else {
        return Value::Null;
    };
    let Some(note) = resolve_note_reference(lookup, &ctx.note.document_path, &link_meta.path)
    else {
        return Value::Null;
    };

    if normalize_field_name(field) == "file" {
        return note_to_file_object(note);
    }
    resolve_property_for_note(note, field)
}

fn resolve_property_for_note(note: &NoteRecord, name: &str) -> Value {
    note.properties
        .as_object()
        .and_then(|props| normalized_object_field(props, name))
        .cloned()
        .unwrap_or(Value::Null)
}

fn folder_distance(source_folder: &str, note_path: &str) -> usize {
    let note_folder = note_path.rsplit_once('/').map_or("", |(folder, _)| folder);
    let source_parts: Vec<&str> = source_folder
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let note_parts: Vec<&str> = note_folder
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let common_prefix = source_parts
        .iter()
        .zip(&note_parts)
        .take_while(|(left, right)| left == right)
        .count();
    source_parts.len() + note_parts.len() - (common_prefix * 2)
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
    resolve_property_for_note(ctx.note, name)
}

fn normalized_object_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Option<&'a Value> {
    object.get(field).or_else(|| {
        let normalized_field = normalize_field_name(field);
        object.iter().find_map(|(key, value)| {
            (normalize_field_name(key) == normalized_field).then_some(value)
        })
    })
}

fn canonical_date_field_name(field: &str) -> String {
    match normalize_field_name(field).as_str() {
        "week-year" => "weekyear".to_string(),
        other => other.to_string(),
    }
}

fn canonical_file_field_name(field: &str) -> String {
    match normalize_field_name(field).as_str() {
        "base-name" => "basename".to_string(),
        "c-day" => "cday".to_string(),
        "c-time" => "ctime".to_string(),
        "front-matter" => "frontmatter".to_string(),
        "in-links" => "inlinks".to_string(),
        "m-day" => "mday".to_string(),
        "m-time" => "mtime".to_string(),
        "out-links" => "outlinks".to_string(),
        other => other.to_string(),
    }
}

fn normalize_field_name(field: &str) -> String {
    let stripped = strip_field_formatting(field);
    let mut normalized = String::new();
    let mut last_was_hyphen = false;

    for ch in stripped.trim().chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                normalized.push(lower);
            }
            last_was_hyphen = false;
        } else if ch.is_whitespace() || ch == '_' || ch == '-' || ch.is_ascii_punctuation() {
            if !normalized.is_empty() && !last_was_hyphen {
                normalized.push('-');
                last_was_hyphen = true;
            }
        } else {
            for lower in ch.to_lowercase() {
                normalized.push(lower);
            }
            last_was_hyphen = false;
        }
    }

    normalized.trim_matches('-').to_string()
}

fn strip_field_formatting(field: &str) -> String {
    let mut stripped = field.trim().to_string();

    loop {
        let next = strip_wrapping_field_formatting(&stripped);
        if next == stripped {
            return stripped;
        }
        stripped = next;
    }
}

fn strip_wrapping_field_formatting(field: &str) -> String {
    for marker in ["**", "__", "~~", "`", "*", "_"] {
        if let Some(inner) = field
            .strip_prefix(marker)
            .and_then(|inner| inner.strip_suffix(marker))
        {
            return inner.trim().to_string();
        }
    }

    field.to_string()
}

pub(crate) fn eval_binary_op(left: &Value, op: BinOp, right: &Value) -> Value {
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
                value.to_string().parse::<i64>().ok()
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
    use serde_json::json;

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
            file_ctime: 1_600_000_000,
            file_size: 1234,
            properties: serde_json::json!({
                "status": "done",
                "count": 42,
                "tags": ["a", "b"],
                "due": "2026-04",
                "due date": "2026-04",
                "estimate": "1d 3h",
                "author": "[[alice]]",
                "Mixed CASE": "loud",
                "snake_case": "yes"
            }),
            tags: vec!["#tag1".to_string(), "#tag2/nested".to_string()],
            links: vec!["other.md".to_string()],
            starred: false,
            inlinks: vec!["[[home]]".to_string()],
            aliases: vec!["Note Alias".to_string()],
            frontmatter: serde_json::json!({"Date": "2026-04-18", "status": "done"}),
            list_items: vec![],
            tasks: vec![],
            raw_inline_expressions: vec![],
            inline_expressions: vec![],
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
        assert_eq!(eval("1 = 1"), Value::Bool(true));
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
    fn eval_task_status_metadata_access() {
        let status_object =
            r#"{"status":"x","statusName":"Done","statusType":"DONE","statusNext":" "}"#;
        assert_eq!(
            eval(&format!("{status_object}.status.symbol")),
            Value::String("x".to_string())
        );
        assert_eq!(
            eval(&format!("{status_object}.status.name")),
            Value::String("Done".to_string())
        );
        assert_eq!(
            eval(&format!("{status_object}.status.type")),
            Value::String("DONE".to_string())
        );
        assert_eq!(
            eval(&format!("{status_object}.status.nextSymbol")),
            Value::String(" ".to_string())
        );
        assert_eq!(eval(&format!("{status_object}.status.length")), json!(1));
    }

    #[test]
    fn eval_field_name_normalization() {
        assert_eq!(eval("DUE-DATE.month"), serde_json::json!(4));
        assert_eq!(eval("mixed-case"), Value::String("loud".to_string()));
        assert_eq!(eval("SNAKE-CASE"), Value::String("yes".to_string()));
        assert_eq!(eval("FILE.NAME"), Value::String("note".to_string()));
        assert_eq!(
            eval(r#"{"Due Date": "2026-04"}["due-date"].month"#),
            serde_json::json!(4)
        );
        assert_eq!(
            eval(r#"{"Mixed CASE": 7}.mixed-case"#),
            serde_json::json!(7)
        );
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
        assert_eq!(eval("file.ctime"), serde_json::json!(1_600_000_000));
        assert_eq!(eval("file.mday"), Value::String("1970-01-20".to_string()));
        assert_eq!(eval("file.cday"), Value::String("1970-01-19".to_string()));
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
        assert_eq!(eval(r#"note["status"]"#), Value::String("done".to_string()));
        assert_eq!(
            eval("file.frontmatter.status"),
            Value::String("done".to_string())
        );
    }

    #[test]
    fn eval_this_binding() {
        assert_eq!(eval("this.status"), Value::String("done".to_string()));
        assert_eq!(eval("this.file.name"), Value::String("note".to_string()));
        assert_eq!(eval("this.file.day.year"), serde_json::json!(2026));
        assert_eq!(
            eval(r#"this["due date"]"#),
            Value::String("2026-04".to_string())
        );
        assert_eq!(
            eval(r#"default(this.missing, "none")"#),
            Value::String("none".to_string())
        );
        assert_eq!(
            eval(r##"contains(this.file.tags, "#tag2")"##),
            Value::Bool(true)
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
            eval(r#"typeof(link("My Project"))"#),
            Value::String("link".to_string())
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
    fn eval_array_swizzling() {
        assert_eq!(
            eval(r#"[{"name": "Ada"}, {"name": "Lin"}].name"#),
            serde_json::json!(["Ada", "Lin"])
        );
        assert_eq!(
            eval(r#"[{"tags": ["a", "b"]}, {"tags": ["c"]}].tags"#),
            serde_json::json!(["a", "b", "c"])
        );
        assert_eq!(
            eval(r#"[{"user": {"name": "Ada"}}, {"user": {"name": "Lin"}}].user.name"#),
            serde_json::json!(["Ada", "Lin"])
        );
        assert_eq!(
            eval(r#"[{"name": "Ada"}, {}].name"#),
            serde_json::json!(["Ada", null])
        );
        assert_eq!(
            eval(r#"[{"name": "Ada"}, {"name": "Lin"}]["name"]"#),
            serde_json::json!(["Ada", "Lin"])
        );
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
            file_ctime: 0,
            file_size: 0,
            properties: serde_json::json!({}),
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
    fn eval_index_access() {
        assert_eq!(eval("[10, 20, 30][1 + 1]"), serde_json::json!(30));
        assert_eq!(eval(r#"{"where": 7}["where"]"#), serde_json::json!(7));
        assert_eq!(eval(r#"file["name"]"#), Value::String("note".to_string()));
        assert_eq!(
            eval(r#"file["aliases"][0]"#),
            Value::String("Note Alias".to_string())
        );
    }

    #[test]
    fn eval_meta_link_fields() {
        assert_eq!(
            eval(r#"meta([[2021-11-01|Displayed link text]]).display"#),
            Value::String("Displayed link text".to_string())
        );
        assert_eq!(
            eval(r#"meta(link("My Project#Next Actions", "Shown")).display"#),
            Value::String("Shown".to_string())
        );
        assert_eq!(
            eval(r#"meta([[My Project#Next Actions]]).path"#),
            Value::String("My Project".to_string())
        );
        assert_eq!(
            eval(r#"meta([[My Project#^9bcbe8]]).subpath"#),
            Value::String("9bcbe8".to_string())
        );
        assert_eq!(
            eval(r#"meta(![[My Project#^9bcbe8]]).embed"#),
            Value::Bool(true)
        );
        assert_eq!(
            eval(r#"meta([[My Project#Next Actions]]).type"#),
            Value::String("header".to_string())
        );
        assert_eq!(
            eval(r#"meta(link("My Project#Next Actions", "Shown")).subpath"#),
            Value::String("Next Actions".to_string())
        );
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
            file_ctime: 1_700_000_000,
            file_size: 1234,
            properties: serde_json::json!({"author": "[[alice]]"}),
            tags: vec![],
            links: vec!["[[alice]]".to_string()],
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
            file_ctime: 1_700_000_001,
            file_size: 500,
            properties: serde_json::json!({"role": "editor"}),
            tags: vec!["#person".to_string()],
            links: vec!["[[note]]".to_string()],
            starred: false,
            inlinks: vec!["[[note]]".to_string()],
            aliases: vec!["Alice".to_string()],
            frontmatter: serde_json::json!({"role": "editor"}),
            list_items: vec![],
            tasks: vec![],
            raw_inline_expressions: vec![],
            inline_expressions: vec![],
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

    #[test]
    fn eval_link_indexing_and_alias_resolution() {
        let alice = NoteRecord {
            document_id: "alice-id".to_string(),
            document_path: "people/alice.md".to_string(),
            file_name: "alice".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1_700_000_001,
            file_ctime: 1_700_000_001,
            file_size: 500,
            properties: serde_json::json!({"role": "editor", "team": "docs", "Display Name": "Alice A."}),
            tags: vec!["#person".to_string()],
            links: vec!["[[note]]".to_string()],
            starred: false,
            inlinks: vec!["[[note]]".to_string()],
            aliases: vec!["Alice".to_string()],
            frontmatter: serde_json::json!({"role": "editor"}),
            list_items: vec![],
            tasks: vec![],
            raw_inline_expressions: vec![],
            inline_expressions: vec![],
        };
        let mut lookup = HashMap::new();
        lookup.insert("alice".to_string(), alice);

        assert_eq!(
            eval_with_lookup("[[alice]].role", &lookup),
            Value::String("editor".to_string())
        );
        assert_eq!(
            eval_with_lookup("[[people/alice]].team", &lookup),
            Value::String("docs".to_string())
        );
        assert_eq!(
            eval_with_lookup(r#"[[Alice]]["role"]"#, &lookup),
            Value::String("editor".to_string())
        );
        assert_eq!(
            eval_with_lookup("[[alice]].file.name", &lookup),
            Value::String("alice".to_string())
        );
        assert_eq!(
            eval_with_lookup("[[Alice]].display-name", &lookup),
            Value::String("Alice A.".to_string())
        );
        assert_eq!(
            eval_with_lookup("[[Alice]].FILE.NAME", &lookup),
            Value::String("alice".to_string())
        );
        assert_eq!(eval_with_lookup("[[nobody]].role", &lookup), Value::Null);
    }
}
