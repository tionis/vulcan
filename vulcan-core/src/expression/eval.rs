use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::expression::ast::{BinOp, Expr, UnOp};
use crate::expression::functions::call_function;
use crate::expression::methods::call_method;
use crate::properties::NoteRecord;

/// Context for evaluating expressions against a single note row.
pub struct EvalContext<'a> {
    pub note: &'a NoteRecord,
    pub formulas: &'a BTreeMap<String, Value>,
    pub now_ms: i64,
    /// Scoped variables for list.filter/map/reduce callbacks.
    pub locals: BTreeMap<String, Value>,
    /// Vault-wide note index keyed by file_name (basename without extension).
    /// Used to resolve `.asFile()` and `.linksTo()` on link values.
    pub note_lookup: Option<&'a HashMap<String, NoteRecord>>,
}

impl<'a> EvalContext<'a> {
    pub fn new(note: &'a NoteRecord, formulas: &'a BTreeMap<String, Value>) -> Self {
        Self {
            note,
            formulas,
            now_ms: now_millis(),
            locals: BTreeMap::new(),
            note_lookup: None,
        }
    }

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
            resolve_property(ctx, name)
        }

        Expr::FieldAccess(receiver, field) => eval_field_access(receiver, field, ctx),

        Expr::FormulaRef(name) => Ok(ctx
            .formulas
            .get(name)
            .cloned()
            .unwrap_or(Value::Null)),

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
            eval_binary_op(&lval, *op, &rval)
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
            return resolve_file_field(ctx, field);
        }
    }

    let receiver_val = evaluate(receiver, ctx)?;

    // Field access on different types
    match &receiver_val {
        // String fields
        Value::String(s) => match field {
            "length" => Ok(Value::Number(s.chars().count().into())),
            _ => Ok(Value::Null),
        },

        // Array fields
        Value::Array(arr) => match field {
            "length" => Ok(Value::Number(arr.len().into())),
            _ => Ok(Value::Null),
        },

        // Object field access
        Value::Object(map) => Ok(map
            .get(field)
            .cloned()
            .unwrap_or(Value::Null)),

        // Number fields (none in Obsidian, but support for consistency)
        _ => Ok(Value::Null),
    }
}

fn resolve_file_field(ctx: &EvalContext, field: &str) -> Result<Value, String> {
    match field {
        "path" => Ok(Value::String(ctx.note.document_path.clone())),
        "name" => Ok(Value::String(ctx.note.file_name.clone())),
        "basename" => Ok(Value::String(ctx.note.file_name.clone())),
        "ext" => Ok(Value::String(ctx.note.file_ext.clone())),
        "folder" => {
            let path = &ctx.note.document_path;
            let folder = path
                .rfind('/')
                .map(|i| &path[..i])
                .unwrap_or("");
            Ok(Value::String(folder.to_string()))
        }
        "size" => Ok(Value::Number(ctx.note.file_size.into())),
        "mtime" => Ok(Value::Number(ctx.note.file_mtime.into())),
        "ctime" => {
            // Use mtime as fallback since ctime is not stored
            Ok(Value::Number(ctx.note.file_mtime.into()))
        }
        "tags" => {
            let tags: Vec<Value> = ctx
                .note
                .tags
                .iter()
                .map(|t| Value::String(t.clone()))
                .collect();
            Ok(Value::Array(tags))
        }
        "links" => {
            let links: Vec<Value> = ctx
                .note
                .links
                .iter()
                .map(|l| Value::String(l.clone()))
                .collect();
            Ok(Value::Array(links))
        }
        "properties" => Ok(ctx.note.properties.clone()),
        _ => Ok(Value::Null),
    }
}

/// Convert a NoteRecord into the file object Value returned by `.asFile()`.
pub fn note_to_file_object(note: &NoteRecord) -> Value {
    let folder = note
        .document_path
        .rfind('/')
        .map(|i| &note.document_path[..i])
        .unwrap_or("");
    let tags: Vec<Value> = note.tags.iter().map(|t| Value::String(t.clone())).collect();
    let links: Vec<Value> = note.links.iter().map(|l| Value::String(l.clone())).collect();
    serde_json::json!({
        "path": note.document_path,
        "name": note.file_name,
        "basename": note.file_name,
        "ext": note.file_ext,
        "folder": folder,
        "size": note.file_size,
        "mtime": note.file_mtime,
        "ctime": note.file_mtime,
        "tags": tags,
        "links": links,
        "properties": note.properties,
    })
}

/// Parse the target filename from a wikilink string like `[[target]]` or `[[target|display]]`.
/// Falls back to the raw string if it is not a wikilink (treating it as a plain filename).
pub fn parse_wikilink_target(s: &str) -> &str {
    let s = s.trim();
    if s.starts_with("[[") && s.ends_with("]]") {
        let inner = &s[2..s.len() - 2];
        // Strip display text
        let inner = inner.find('|').map(|i| &inner[..i]).unwrap_or(inner);
        // Strip heading/block anchor
        let inner = inner.find('#').map(|i| &inner[..i]).unwrap_or(inner);
        inner.trim()
    } else {
        s
    }
}

fn call_file_method(method: &str, args: &[crate::expression::ast::Expr], ctx: &EvalContext) -> Result<Value, String> {
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
                .map(|m| m.contains_key(prop) && m.get(prop) != Some(&Value::Null))
                .unwrap_or(false);
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
            let basename = path
                .rfind('/')
                .map(|i| &path[i + 1..])
                .unwrap_or(path);
            let basename = basename.trim_end_matches(".md");
            Ok(Value::String(format!("[[{basename}]]")))
        }
        _ => Err(format!("unknown file method `{method}`")),
    }
}

fn resolve_property(ctx: &EvalContext, name: &str) -> Result<Value, String> {
    if let Some(props) = ctx.note.properties.as_object() {
        Ok(props.get(name).cloned().unwrap_or(Value::Null))
    } else {
        Ok(Value::Null)
    }
}

fn eval_binary_op(left: &Value, op: BinOp, right: &Value) -> Result<Value, String> {
    match op {
        BinOp::Add => eval_add(left, right),
        BinOp::Sub => eval_sub(left, right),
        BinOp::Mul => eval_arithmetic(left, right, |a, b| a * b),
        BinOp::Div => {
            let b = as_number(right);
            if b == 0.0 {
                return Ok(Value::Null);
            }
            eval_arithmetic(left, right, |a, b| a / b)
        }
        BinOp::Eq => Ok(Value::Bool(values_equal(left, right))),
        BinOp::Ne => Ok(Value::Bool(!values_equal(left, right))),
        BinOp::Gt => Ok(Value::Bool(compare_values(left, right) == Some(std::cmp::Ordering::Greater))),
        BinOp::Lt => Ok(Value::Bool(compare_values(left, right) == Some(std::cmp::Ordering::Less))),
        BinOp::Ge => Ok(Value::Bool(matches!(
            compare_values(left, right),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ))),
        BinOp::Le => Ok(Value::Bool(matches!(
            compare_values(left, right),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        ))),
        BinOp::And | BinOp::Or => unreachable!("handled by short-circuit"),
    }
}

fn eval_add(left: &Value, right: &Value) -> Result<Value, String> {
    use crate::expression::functions::parse_duration_string;
    // Date arithmetic: number + duration_string (e.g. now() + "7d")
    if let (Value::Number(_), Value::String(s)) = (left, right) {
        if let Some(ms) = parse_duration_string(s) {
            let n = as_number(left);
            return Ok(number_to_value(n + ms as f64));
        }
    }
    if let (Value::String(s), Value::Number(_)) = (left, right) {
        if let Some(ms) = parse_duration_string(s) {
            let n = as_number(right);
            return Ok(number_to_value(ms as f64 + n));
        }
    }
    // String concatenation if either side is a string
    match (left, right) {
        (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{a}{b}"))),
        (Value::String(a), _) => Ok(Value::String(format!("{a}{}", value_to_display(right)))),
        (_, Value::String(b)) => Ok(Value::String(format!("{}{b}", value_to_display(left)))),
        _ => eval_arithmetic(left, right, |a, b| a + b),
    }
}

fn eval_sub(left: &Value, right: &Value) -> Result<Value, String> {
    use crate::expression::functions::parse_duration_string;
    // Date arithmetic: number - duration_string (e.g. now() - "7d")
    if let (Value::Number(_), Value::String(s)) = (left, right) {
        if let Some(ms) = parse_duration_string(s) {
            let n = as_number(left);
            return Ok(number_to_value(n - ms as f64));
        }
    }
    eval_arithmetic(left, right, |a, b| a - b)
}

fn eval_arithmetic(left: &Value, right: &Value, f: fn(f64, f64) -> f64) -> Result<Value, String> {
    let a = as_number(left);
    let b = as_number(right);
    if a.is_nan() || b.is_nan() {
        return Ok(Value::Null);
    }
    Ok(number_to_value(f(a, b)))
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => a == b,
        _ => false,
    }
}

fn compare_values(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => {
            a.as_f64().unwrap_or(0.0).partial_cmp(&b.as_f64().unwrap_or(0.0))
        }
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

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

pub fn as_number(value: &Value) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::Bool(true) => 1.0,
        Value::Bool(false) => 0.0,
        Value::String(s) => s.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

pub fn value_to_display(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f == f.trunc() && f.abs() < 1e15 {
                format!("{}", f as i64)
            } else {
                n.to_string()
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

pub fn number_to_value(n: f64) -> Value {
    if n == n.trunc() && n.abs() < (i64::MAX as f64) {
        Value::Number(serde_json::Number::from(n as i64))
    } else {
        serde_json::Number::from_f64(n)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

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
        let expr = Parser::new(input).unwrap().parse().unwrap();
        let note = NoteRecord {
            document_path: "folder/note.md".to_string(),
            file_name: "note".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1700000000,
            file_size: 1234,
            properties: serde_json::json!({"status": "done", "count": 42, "tags": ["a", "b"]}),
            tags: vec!["tag1".to_string(), "tag2".to_string()],
            links: vec!["other.md".to_string()],
        };
        let formulas = BTreeMap::new();
        let ctx = EvalContext::new(&note, &formulas);
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
        assert_eq!(
            eval("file.name"),
            Value::String("note".to_string())
        );
        assert_eq!(
            eval("file.basename"),
            Value::String("note".to_string())
        );
        assert_eq!(eval("file.ext"), Value::String("md".to_string()));
        assert_eq!(eval("file.folder"), Value::String("folder".to_string()));
        assert_eq!(eval("file.size"), serde_json::json!(1234));
        assert_eq!(eval("file.mtime"), serde_json::json!(1700000000));
    }

    #[test]
    fn eval_file_tags_and_links() {
        assert_eq!(
            eval("file.tags"),
            serde_json::json!(["tag1", "tag2"])
        );
        assert_eq!(
            eval("file.links"),
            serde_json::json!(["other.md"])
        );
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
            document_path: "test.md".to_string(),
            file_name: "test".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 0,
            file_size: 0,
            properties: serde_json::json!({}),
            tags: vec![],
            links: vec![],
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
            document_path: "folder/note.md".to_string(),
            file_name: "note".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1700000000,
            file_size: 1234,
            properties: serde_json::json!({"author": "[[alice]]"}),
            tags: vec![],
            links: vec!["[[alice]]".to_string()],
        };
        let formulas = BTreeMap::new();
        let ctx = EvalContext::new(&note, &formulas).with_note_lookup(lookup);
        evaluate(&expr, &ctx).unwrap()
    }

    #[test]
    fn eval_as_file_and_links_to() {
        let alice = NoteRecord {
            document_path: "people/alice.md".to_string(),
            file_name: "alice".to_string(),
            file_ext: "md".to_string(),
            file_mtime: 1700000001,
            file_size: 500,
            properties: serde_json::json!({"role": "editor"}),
            tags: vec!["person".to_string()],
            links: vec!["[[note]]".to_string()],
        };
        let mut lookup = HashMap::new();
        lookup.insert("alice".to_string(), alice);

        // asFile() on a wikilink returns the file object
        let file_obj = eval_with_lookup(r#""[[alice]]".asFile()"#, &lookup);
        assert_eq!(file_obj.get("name").unwrap(), &Value::String("alice".to_string()));
        assert_eq!(file_obj.get("ext").unwrap(), &Value::String("md".to_string()));
        assert_eq!(file_obj.get("properties").unwrap().get("role").unwrap(), &Value::String("editor".to_string()));

        // asFile() on unknown link returns null
        assert_eq!(eval_with_lookup(r#""[[nobody]]".asFile()"#, &lookup), Value::Null);

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
