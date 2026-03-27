use regex::Regex;
use serde_json::Value;

use crate::expression::ast::{BinOp, Expr};
use crate::expression::eval::{
    as_number, compare_values, eval_binary_op, evaluate, is_truthy, number_to_value,
    value_to_display, EvalContext,
};
use crate::expression::methods::{call_method, evaluate_callback};

#[allow(clippy::too_many_lines)]
pub fn call_function(name: &str, args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    match name {
        "if" => func_if(args, ctx),
        "now" => Ok(Value::Number(ctx.now_ms.into())),
        "today" => Ok(Value::Number(start_of_day(ctx.now_ms).into())),
        "date" => func_date(args, ctx),
        "meta" => func_meta(args, ctx),
        "typeof" => func_typeof(args, ctx),
        "length" => func_length(args, ctx),
        "object" => func_object(args, ctx),
        "round" => func_round(args, ctx),
        "trunc" => func_numeric_unary(args, ctx, f64::trunc),
        "floor" => func_numeric_unary(args, ctx, f64::floor),
        "ceil" => func_numeric_unary(args, ctx, f64::ceil),
        "default" => func_default(args, ctx, true),
        "ldefault" => func_default(args, ctx, false),
        "choice" => func_choice(args, ctx),
        "contains" => func_contains(args, ctx, ContainsMode::Recursive),
        "icontains" => func_contains(args, ctx, ContainsMode::Insensitive),
        "econtains" => func_contains(args, ctx, ContainsMode::Exact),
        "containsword" => func_contains_word(args, ctx),
        "nonnull" => func_nonnull(args, ctx),
        "firstvalue" => func_firstvalue(args, ctx),
        "join" => func_join(args, ctx),
        "all" => func_truthy_aggregate(args, ctx, TruthyAggregate::All),
        "any" => func_truthy_aggregate(args, ctx, TruthyAggregate::Any),
        "none" => func_truthy_aggregate(args, ctx, TruthyAggregate::None),
        "lower" => func_string_map(args, ctx, |text| text.to_lowercase()),
        "upper" => func_string_map(args, ctx, |text| text.to_uppercase()),
        "startswith" => func_string_predicate(args, ctx, |text, prefix| text.starts_with(prefix)),
        "endswith" => func_string_predicate(args, ctx, |text, suffix| text.ends_with(suffix)),
        "substring" => func_substring(args, ctx),
        "split" => func_split(args, ctx),
        "replace" => func_replace(args, ctx),
        "regextest" => func_regextest(args, ctx),
        "regexmatch" => func_regexmatch(args, ctx),
        "regexreplace" => func_regexreplace(args, ctx),
        "truncate" => func_truncate(args, ctx),
        "padleft" => func_pad(args, ctx, true),
        "padright" => func_pad(args, ctx, false),
        "map" | "filter" | "sort" | "reverse" | "unique" | "flat" | "slice" => {
            func_array_alias(name, args, ctx)
        }
        "number" => {
            let val = eval_arg(args, 0, ctx)?;
            let n = as_number(&val);
            if n.is_nan() {
                Ok(Value::Null)
            } else {
                Ok(number_to_value(n))
            }
        }
        "max" => func_extreme(args, ctx, true),
        "min" => func_extreme(args, ctx, false),
        "sum" => func_aggregate(args, ctx, "+"),
        "product" => func_aggregate(args, ctx, "*"),
        "average" => func_average(args, ctx),
        "reduce" => func_reduce(args, ctx),
        "minby" => func_extreme_by(args, ctx, false),
        "maxby" => func_extreme_by(args, ctx, true),
        "link" => {
            let path = eval_arg(args, 0, ctx)?;
            let display = if args.len() > 1 {
                Some(value_to_display(&eval_arg(args, 1, ctx)?))
            } else {
                None
            };
            let path_str = value_to_display(&path);
            match display {
                Some(d) => Ok(Value::String(format!("[[{path_str}|{d}]]"))),
                None => Ok(Value::String(format!("[[{path_str}]]"))),
            }
        }
        "list" => {
            let values = eval_all_args(args, ctx)?;
            match values.as_slice() {
                [Value::Array(_)] => Ok(values.into_iter().next().unwrap_or(Value::Array(vec![]))),
                _ => Ok(Value::Array(values)),
            }
        }
        "escapeHTML" => {
            let val = eval_arg(args, 0, ctx)?;
            let s = value_to_display(&val);
            Ok(Value::String(
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;")
                    .replace('\'', "&#39;"),
            ))
        }
        "html" | "image" | "icon" => {
            let val = eval_arg(args, 0, ctx)?;
            Ok(Value::String(value_to_display(&val)))
        }
        "file" => {
            // Returns the path as a string; full file-object support deferred
            let val = eval_arg(args, 0, ctx)?;
            Ok(Value::String(value_to_display(&val)))
        }
        "duration" | "dur" => func_duration(args, ctx),
        _ => Err(format!("unknown function `{name}`")),
    }
}

#[derive(Clone, Copy)]
enum ContainsMode {
    Recursive,
    Insensitive,
    Exact,
}

#[derive(Clone, Copy)]
enum TruthyAggregate {
    All,
    Any,
    None,
}

fn func_if(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::Null);
    }
    let condition = evaluate(&args[0], ctx)?;
    if is_truthy(&condition) {
        if args.len() > 1 {
            evaluate(&args[1], ctx)
        } else {
            Ok(Value::Null)
        }
    } else if args.len() > 2 {
        evaluate(&args[2], ctx)
    } else {
        Ok(Value::Null)
    }
}

fn func_date(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    if let Some(shortcut_ms) = args.first().and_then(|expr| date_shortcut_arg(expr, ctx)) {
        return Ok(Value::Number(shortcut_ms.into()));
    }

    let val = eval_arg(args, 0, ctx)?;
    match &val {
        Value::String(s) => {
            Ok(parse_date_like_string(s).map_or(Value::Null, |ms| Value::Number(ms.into())))
        }
        Value::Number(_) => Ok(val),
        _ => Ok(Value::Null),
    }
}

fn func_duration(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let val = eval_arg(args, 0, ctx)?;
    match &val {
        Value::String(s) => {
            Ok(parse_duration_string(s).map_or(Value::Null, |ms| Value::Number(ms.into())))
        }
        _ => Ok(Value::Null),
    }
}

fn func_typeof(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let Some(expr) = args.first() else {
        return Ok(Value::String("null".to_string()));
    };

    if let Some(type_name) = type_name_hint(expr) {
        return Ok(Value::String(type_name.to_string()));
    }

    let value = evaluate(expr, ctx)?;
    Ok(Value::String(value_type_name(&value).to_string()))
}

fn func_meta(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    Ok(link_meta_value(&value))
}

fn func_length(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let length = match value {
        Value::Array(values) => values.len(),
        Value::Object(map) => map.len(),
        Value::String(text) => text.chars().count(),
        Value::Null => 0,
        _ => return Ok(Value::Null),
    };
    Ok(Value::Number(length.into()))
}

fn func_object(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    if args.len() % 2 != 0 {
        return Err("object() requires an even number of arguments".to_string());
    }

    let mut map = serde_json::Map::new();
    let mut index = 0;
    while index < args.len() {
        let key = eval_arg(args, index, ctx)?;
        let Value::String(key) = key else {
            return Err("object() keys must evaluate to strings".to_string());
        };
        map.insert(key, eval_arg(args, index + 1, ctx)?);
        index += 2;
    }
    Ok(Value::Object(map))
}

fn func_round(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let number = as_number(&value);
    if value.is_null() || number.is_nan() {
        return Ok(Value::Null);
    }

    let precision_value = eval_arg(args, 1, ctx)?;
    let precision = as_number(&precision_value);
    if args.len() < 2 || precision_value.is_null() || precision.is_nan() || precision <= 0.0 {
        return Ok(number_to_value(number.round()));
    }

    let factor = 10_f64.powi(precision as i32);
    Ok(number_to_value((number * factor).round() / factor))
}

fn func_numeric_unary(
    args: &[Expr],
    ctx: &EvalContext,
    op: fn(f64) -> f64,
) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let number = as_number(&value);
    if value.is_null() || number.is_nan() {
        return Ok(Value::Null);
    }
    Ok(number_to_value(op(number)))
}

fn func_default(args: &[Expr], ctx: &EvalContext, vectorize: bool) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let fallback = eval_arg(args, 1, ctx)?;
    Ok(default_value(value, fallback, vectorize))
}

fn func_choice(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::Null);
    }

    let condition = eval_arg(args, 0, ctx)?;
    if is_truthy(&condition) {
        eval_arg(args, 1, ctx)
    } else {
        eval_arg(args, 2, ctx)
    }
}

fn func_contains(args: &[Expr], ctx: &EvalContext, mode: ContainsMode) -> Result<Value, String> {
    let haystack = eval_arg(args, 0, ctx)?;
    let needle = eval_arg(args, 1, ctx)?;
    Ok(Value::Bool(contains_value(&haystack, &needle, mode)))
}

fn func_contains_word(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let haystack = eval_arg(args, 0, ctx)?;
    let needle = eval_arg(args, 1, ctx)?;

    Ok(match haystack {
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| contains_word_value(&value, &needle))
                .collect(),
        ),
        _ => contains_word_value(&haystack, &needle),
    })
}

fn func_string_map(
    args: &[Expr],
    ctx: &EvalContext,
    transform: fn(&str) -> String,
) -> Result<Value, String> {
    Ok(map_string_value(eval_arg(args, 0, ctx)?, transform))
}

fn func_string_predicate(
    args: &[Expr],
    ctx: &EvalContext,
    predicate: fn(&str, &str) -> bool,
) -> Result<Value, String> {
    let haystack = eval_arg(args, 0, ctx)?;
    let needle = eval_arg(args, 1, ctx)?;
    Ok(match (&haystack, &needle) {
        (Value::String(haystack), Value::String(needle)) => {
            Value::Bool(predicate(haystack, needle))
        }
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        _ => Value::Null,
    })
}

fn func_substring(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let Value::String(text) = value else {
        return Ok(if value.is_null() {
            Value::Null
        } else {
            Value::Null
        });
    };

    let start = eval_nonnegative_index(args, 1, ctx)?;
    let end = if args.len() > 2 {
        Some(eval_nonnegative_index(args, 2, ctx)?)
    } else {
        None
    };

    Ok(Value::String(substring_value(&text, start, end)))
}

fn func_split(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let delimiter = eval_arg(args, 1, ctx)?;
    let (Value::String(text), Value::String(pattern)) = (&value, &delimiter) else {
        return Ok(if value.is_null() || delimiter.is_null() {
            Value::Null
        } else {
            Value::Null
        });
    };

    let limit = if args.len() > 2 {
        Some(eval_nonnegative_index(args, 2, ctx)?)
    } else {
        None
    };

    Ok(Value::Array(regex_split(text, pattern, limit)?))
}

fn func_replace(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let pattern = eval_arg(args, 1, ctx)?;
    let replacement = eval_arg(args, 2, ctx)?;
    let (Value::String(text), Value::String(pattern), Value::String(replacement)) =
        (&value, &pattern, &replacement)
    else {
        return Ok(
            if value.is_null() || pattern.is_null() || replacement.is_null() {
                Value::Null
            } else {
                Value::Null
            },
        );
    };

    Ok(Value::String(text.replace(pattern, replacement)))
}

fn func_regextest(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let pattern = eval_arg(args, 0, ctx)?;
    let value = eval_arg(args, 1, ctx)?;
    Ok(Value::Bool(match (&pattern, &value) {
        (Value::String(pattern), Value::String(text)) => {
            compile_regex(pattern, "regextest")?.is_match(text)
        }
        _ => false,
    }))
}

fn func_regexmatch(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let pattern = eval_arg(args, 0, ctx)?;
    let value = eval_arg(args, 1, ctx)?;
    Ok(Value::Bool(match (&pattern, &value) {
        (Value::String(pattern), Value::String(text)) => {
            compile_regex(&anchored_pattern(pattern), "regexmatch")?.is_match(text)
        }
        _ => false,
    }))
}

fn func_regexreplace(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let pattern = eval_arg(args, 1, ctx)?;
    let replacement = eval_arg(args, 2, ctx)?;
    let (Value::String(text), Value::String(pattern), Value::String(replacement)) =
        (&value, &pattern, &replacement)
    else {
        return Ok(
            if value.is_null() || pattern.is_null() || replacement.is_null() {
                Value::Null
            } else {
                Value::Null
            },
        );
    };

    let regex = compile_regex(pattern, "regexreplace")?;
    Ok(Value::String(
        regex.replace_all(text, replacement).into_owned(),
    ))
}

fn func_truncate(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let Value::String(text) = value else {
        return Ok(if value.is_null() {
            Value::Null
        } else {
            Value::Null
        });
    };

    let length = eval_nonnegative_index(args, 1, ctx)?;
    let suffix = match eval_arg(args, 2, ctx)? {
        Value::String(suffix) => suffix,
        Value::Null if args.len() < 3 => "...".to_string(),
        Value::Null => return Ok(Value::Null),
        _ => return Ok(Value::Null),
    };

    Ok(Value::String(truncate_value(&text, length, &suffix)))
}

fn func_pad(args: &[Expr], ctx: &EvalContext, left: bool) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let Value::String(text) = value else {
        return Ok(if value.is_null() {
            Value::Null
        } else {
            Value::Null
        });
    };

    let target_length = eval_nonnegative_index(args, 1, ctx)?;
    let padding = match eval_arg(args, 2, ctx)? {
        Value::String(padding) if !padding.is_empty() => padding,
        Value::Null if args.len() < 3 => " ".to_string(),
        Value::Null => return Ok(Value::Null),
        _ => return Ok(Value::Null),
    };

    Ok(Value::String(pad_string(
        &text,
        target_length,
        &padding,
        left,
    )))
}

fn func_nonnull(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let values = eval_all_args(args, ctx)?;
    match values.as_slice() {
        [Value::Array(items)] => Ok(Value::Array(
            items
                .iter()
                .filter(|value| !value.is_null())
                .cloned()
                .collect(),
        )),
        _ => Ok(Value::Array(
            values
                .into_iter()
                .filter(|value| !value.is_null())
                .collect(),
        )),
    }
}

fn func_firstvalue(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let values = eval_all_args(args, ctx)?;
    let first = match values.as_slice() {
        [Value::Array(items)] => items.iter().find(|value| !value.is_null()).cloned(),
        _ => values.into_iter().find(|value| !value.is_null()),
    };
    Ok(first.unwrap_or(Value::Null))
}

fn func_join(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    match value {
        Value::Array(_) => call_method(&value, "join", &args[1..], ctx),
        _ => Ok(Value::String(value_to_display(&value))),
    }
}

fn func_truthy_aggregate(
    args: &[Expr],
    ctx: &EvalContext,
    aggregate: TruthyAggregate,
) -> Result<Value, String> {
    if args.is_empty() {
        return Ok(Value::Bool(matches!(
            aggregate,
            TruthyAggregate::All | TruthyAggregate::None
        )));
    }

    if args.len() <= 2 {
        let first = eval_arg(args, 0, ctx)?;
        if let Value::Array(values) = first {
            return if args.len() == 1 {
                Ok(Value::Bool(apply_truthy_aggregate(
                    values.iter().map(is_truthy),
                    aggregate,
                )))
            } else {
                func_truthy_aggregate_with_callback(values, &args[1], ctx, aggregate)
            };
        }
    }

    let values = eval_all_args(args, ctx)?;
    Ok(Value::Bool(apply_truthy_aggregate(
        values.iter().map(is_truthy),
        aggregate,
    )))
}

fn func_truthy_aggregate_with_callback(
    values: Vec<Value>,
    callback: &Expr,
    ctx: &EvalContext,
    aggregate: TruthyAggregate,
) -> Result<Value, String> {
    let mut results = Vec::with_capacity(values.len());
    for (index, value) in values.into_iter().enumerate() {
        let result = evaluate_callback(
            callback,
            ctx,
            &[
                ("value", value.clone()),
                ("index", Value::Number(index.into())),
            ],
            &[value, Value::Number(index.into())],
        )?;
        results.push(result);
    }

    Ok(Value::Bool(apply_truthy_aggregate(
        results.iter().map(is_truthy),
        aggregate,
    )))
}

fn func_extreme(args: &[Expr], ctx: &EvalContext, pick_max: bool) -> Result<Value, String> {
    let values = eval_variadic_values(args, ctx)?;
    Ok(extreme_value(values, pick_max))
}

fn func_aggregate(args: &[Expr], ctx: &EvalContext, operator: &str) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    match value {
        Value::Array(values) => reduce_values_by_operator(values, operator),
        other => Ok(other),
    }
}

fn func_average(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let Value::Array(values) = value else {
        return Ok(value);
    };

    if values.is_empty() {
        return Ok(Value::Null);
    }

    let count = values.len();
    let sum = reduce_values_by_operator(values, "+")?;
    if sum.is_null() {
        return Ok(Value::Null);
    }

    Ok(eval_binary_op(
        &sum,
        BinOp::Div,
        &Value::Number(count.into()),
    ))
}

fn func_reduce(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let Value::Array(values) = eval_arg(args, 0, ctx)? else {
        return Ok(Value::Null);
    };
    let Some(operand_expr) = args.get(1) else {
        return Ok(Value::Null);
    };
    if values.is_empty() {
        return Ok(Value::Null);
    }

    match operand_expr {
        Expr::Str(operator) => reduce_values_by_operator(values, operator),
        _ => reduce_values_by_callback(values, operand_expr, ctx),
    }
}

fn func_extreme_by(args: &[Expr], ctx: &EvalContext, pick_max: bool) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let Value::Array(values) = value else {
        return Ok(Value::Null);
    };
    let Some(callback) = args.get(1) else {
        return Ok(Value::Null);
    };
    if values.is_empty() {
        return Ok(Value::Null);
    }

    let first = values.first().cloned().unwrap_or(Value::Null);
    let mut mapped = Vec::new();
    for (index, item) in values.into_iter().enumerate() {
        let mapped_value = evaluate_callback(
            callback,
            ctx,
            &[
                ("value", item.clone()),
                ("index", Value::Number(index.into())),
            ],
            &[item.clone(), Value::Number(index.into())],
        )?;
        if !mapped_value.is_null() {
            mapped.push((item, mapped_value));
        }
    }

    let mut mapped_iter = mapped.into_iter();
    let Some((mut best_item, mut best_key)) = mapped_iter.next() else {
        return Ok(first);
    };

    for (item, key) in mapped_iter {
        let ordering = compare_values(&best_key, &key);
        let keep_candidate = if pick_max {
            ordering == Some(std::cmp::Ordering::Less)
        } else {
            ordering == Some(std::cmp::Ordering::Greater)
        };
        if keep_candidate {
            best_item = item;
            best_key = key;
        }
    }

    Ok(best_item)
}

fn func_array_alias(name: &str, args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let Some((receiver_expr, method_args)) = args.split_first() else {
        return Ok(Value::Null);
    };

    let receiver = evaluate(receiver_expr, ctx)?;
    if receiver.is_null() {
        return Ok(Value::Null);
    }
    call_method(&receiver, name, method_args, ctx)
}

fn default_value(value: Value, fallback: Value, vectorize: bool) -> Value {
    if !vectorize {
        return if value.is_null() { fallback } else { value };
    }

    match (value, fallback) {
        (Value::Array(values), Value::Array(fallbacks)) => Value::Array(
            values
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    default_value(
                        value,
                        fallbacks.get(index).cloned().unwrap_or(Value::Null),
                        true,
                    )
                })
                .collect(),
        ),
        (Value::Array(values), fallback) => Value::Array(
            values
                .into_iter()
                .map(|value| default_value(value, fallback.clone(), true))
                .collect(),
        ),
        (value, Value::Array(fallbacks)) => Value::Array(
            fallbacks
                .into_iter()
                .map(|fallback| default_value(value.clone(), fallback, true))
                .collect(),
        ),
        (value, fallback) => {
            if value.is_null() {
                fallback
            } else {
                value
            }
        }
    }
}

fn eval_variadic_values(args: &[Expr], ctx: &EvalContext) -> Result<Vec<Value>, String> {
    let values = eval_all_args(args, ctx)?;
    Ok(match values.as_slice() {
        [Value::Array(items)] => items.clone(),
        _ => values,
    })
}

fn eval_arg(args: &[Expr], index: usize, ctx: &EvalContext) -> Result<Value, String> {
    args.get(index)
        .map_or(Ok(Value::Null), |e| evaluate(e, ctx))
}

fn eval_all_args(args: &[Expr], ctx: &EvalContext) -> Result<Vec<Value>, String> {
    args.iter().map(|e| evaluate(e, ctx)).collect()
}

fn contains_value(haystack: &Value, needle: &Value, mode: ContainsMode) -> bool {
    match mode {
        ContainsMode::Recursive => contains_recursive(haystack, needle, false),
        ContainsMode::Insensitive => contains_recursive(haystack, needle, true),
        ContainsMode::Exact => contains_exact(haystack, needle),
    }
}

fn contains_recursive(haystack: &Value, needle: &Value, case_insensitive: bool) -> bool {
    match haystack {
        Value::Array(values) => values
            .iter()
            .any(|value| contains_recursive(value, needle, case_insensitive)),
        Value::String(text) => needle.as_str().is_some_and(|needle| {
            if case_insensitive {
                text.to_lowercase().contains(&needle.to_lowercase())
            } else {
                text.contains(needle)
            }
        }),
        Value::Object(map) => needle.as_str().is_some_and(|needle| {
            if case_insensitive {
                let needle = needle.to_lowercase();
                map.keys().any(|key| key.to_lowercase() == needle)
            } else {
                map.contains_key(needle)
            }
        }),
        _ => values_match(haystack, needle),
    }
}

fn contains_exact(haystack: &Value, needle: &Value) -> bool {
    match haystack {
        Value::Array(values) => values.iter().any(|value| values_match(value, needle)),
        Value::String(text) => needle.as_str().is_some_and(|needle| text.contains(needle)),
        Value::Object(map) => needle
            .as_str()
            .is_some_and(|needle| map.contains_key(needle)),
        _ => values_match(haystack, needle),
    }
}

fn contains_word_value(haystack: &Value, needle: &Value) -> Value {
    let (Value::String(text), Value::String(needle)) = (haystack, needle) else {
        return Value::Null;
    };
    let needle = needle.to_lowercase();
    let contains = text
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| word.to_lowercase() == needle);
    Value::Bool(contains)
}

fn apply_truthy_aggregate<I>(mut values: I, aggregate: TruthyAggregate) -> bool
where
    I: Iterator<Item = bool>,
{
    match aggregate {
        TruthyAggregate::All => values.all(std::convert::identity),
        TruthyAggregate::Any => values.any(std::convert::identity),
        TruthyAggregate::None => !values.any(std::convert::identity),
    }
}

fn values_match(left: &Value, right: &Value) -> bool {
    compare_values(left, right) == Some(std::cmp::Ordering::Equal) || left == right
}

fn map_string_value(value: Value, transform: fn(&str) -> String) -> Value {
    match value {
        Value::String(text) => Value::String(transform(&text)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| map_string_value(value, transform))
                .collect(),
        ),
        Value::Null => Value::Null,
        _ => Value::Null,
    }
}

fn eval_nonnegative_index(args: &[Expr], index: usize, ctx: &EvalContext) -> Result<usize, String> {
    let value = eval_arg(args, index, ctx)?;
    let number = as_number(&value);
    if value.is_null() || number.is_nan() {
        return Ok(0);
    }
    Ok(number.max(0.0) as usize)
}

fn anchored_pattern(pattern: &str) -> String {
    if !pattern.starts_with('^') && !pattern.ends_with('$') {
        format!("^{pattern}$")
    } else {
        pattern.to_string()
    }
}

fn compile_regex(pattern: &str, function: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|error| format!("invalid regex for {function}(): {error}"))
}

fn regex_split(text: &str, pattern: &str, limit: Option<usize>) -> Result<Vec<Value>, String> {
    let regex = compile_regex(pattern, "split")?;
    let max_items = limit.unwrap_or(usize::MAX);
    if max_items == 0 {
        return Ok(vec![]);
    }

    let mut result = Vec::new();
    let mut last_end = 0;

    for captures in regex.captures_iter(text) {
        let Some(matched) = captures.get(0) else {
            continue;
        };

        if result.len() >= max_items {
            break;
        }
        result.push(Value::String(text[last_end..matched.start()].to_string()));
        if result.len() >= max_items {
            break;
        }

        for index in 1..captures.len() {
            result.push(Value::String(
                captures
                    .get(index)
                    .map_or_else(String::new, |capture| capture.as_str().to_string()),
            ));
            if result.len() >= max_items {
                break;
            }
        }
        if result.len() >= max_items {
            break;
        }

        last_end = matched.end();
    }

    if result.len() < max_items {
        result.push(Value::String(text[last_end..].to_string()));
    }
    result.truncate(max_items);
    Ok(result)
}

fn substring_value(text: &str, start: usize, end: Option<usize>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = start.min(chars.len());
    let end = end.unwrap_or(chars.len()).min(chars.len());
    chars[start..end.max(start)].iter().collect()
}

fn truncate_value(text: &str, length: usize, suffix: &str) -> String {
    let text_len = text.chars().count();
    if text_len <= length {
        return text.to_string();
    }

    let suffix_len = suffix.chars().count();
    let keep = length.saturating_sub(suffix_len);
    format!("{}{}", substring_value(text, 0, Some(keep)), suffix)
}

fn pad_string(text: &str, target_length: usize, padding: &str, left: bool) -> String {
    let text_len = text.chars().count();
    if text_len >= target_length {
        return text.to_string();
    }

    let pad_len = target_length - text_len;
    let fill = repeated_fill(padding, pad_len);
    if left {
        format!("{fill}{text}")
    } else {
        format!("{text}{fill}")
    }
}

fn repeated_fill(padding: &str, target_length: usize) -> String {
    let padding_chars: Vec<char> = padding.chars().collect();
    if padding_chars.is_empty() {
        return String::new();
    }

    let mut result = String::new();
    for index in 0..target_length {
        result.push(padding_chars[index % padding_chars.len()]);
    }
    result
}

fn extreme_value(values: Vec<Value>, pick_max: bool) -> Value {
    let mut iter = values.into_iter();
    let Some(mut best) = iter.next() else {
        return Value::Null;
    };

    for candidate in iter {
        best = choose_extreme(best, candidate, pick_max);
    }

    best
}

fn choose_extreme(left: Value, right: Value, pick_max: bool) -> Value {
    match (&left, &right) {
        (Value::Null, _) => return right,
        (_, Value::Null) => return left,
        _ => {}
    }

    match compare_values(&left, &right) {
        Some(std::cmp::Ordering::Less) => {
            if pick_max {
                right
            } else {
                left
            }
        }
        Some(std::cmp::Ordering::Greater) => {
            if pick_max {
                left
            } else {
                right
            }
        }
        Some(std::cmp::Ordering::Equal) | None => left,
    }
}

fn reduce_values_by_operator(values: Vec<Value>, operator: &str) -> Result<Value, String> {
    let mut iter = values.into_iter();
    let Some(mut acc) = iter.next() else {
        return Ok(Value::Null);
    };

    for value in iter {
        acc = match operator {
            "+" => eval_binary_op(&acc, BinOp::Add, &value),
            "-" => eval_binary_op(&acc, BinOp::Sub, &value),
            "*" => eval_binary_op(&acc, BinOp::Mul, &value),
            "/" => eval_binary_op(&acc, BinOp::Div, &value),
            "&" => Value::Bool(is_truthy(&acc) && is_truthy(&value)),
            "|" => Value::Bool(is_truthy(&acc) || is_truthy(&value)),
            _ => {
                return Err(
                    "reduce(array, op) supports '+', '-', '/', '*', '&', and '|'".to_string(),
                )
            }
        };
    }

    Ok(acc)
}

fn reduce_values_by_callback(
    values: Vec<Value>,
    callback: &Expr,
    ctx: &EvalContext,
) -> Result<Value, String> {
    let mut iter = values.into_iter().enumerate();
    let Some((_, mut acc)) = iter.next() else {
        return Ok(Value::Null);
    };

    for (index, item) in iter {
        if item.is_null() {
            continue;
        }
        acc = evaluate_callback(
            callback,
            ctx,
            &[
                ("acc", acc.clone()),
                ("value", item.clone()),
                ("index", Value::Number(index.into())),
            ],
            &[acc, item, Value::Number(index.into())],
        )?;
    }

    Ok(acc)
}

/// Parse an ISO 8601 date string into milliseconds since epoch.
#[must_use]
pub fn parse_date_string(s: &str) -> Option<i64> {
    let s = s.trim();
    // Try YYYY-MM-DD HH:mm:ss or YYYY-MM-DD
    let (date_part, time_part) = if let Some((d, t)) = s.split_once(' ') {
        (d, Some(t))
    } else if let Some((d, t)) = s.split_once('T') {
        (d, Some(t))
    } else {
        (s, None)
    };

    let mut parts = date_part.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (hour, minute, second) = if let Some(t) = time_part {
        let t = t.trim_end_matches('Z');
        let mut tparts = t.split(':');
        let h: i64 = tparts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let m: i64 = tparts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let sec: i64 = tparts
            .next()
            .and_then(|s| s.split('.').next()?.parse().ok())
            .unwrap_or(0);
        (h, m, sec)
    } else {
        (0, 0, 0)
    };

    // Simplified days-since-epoch calculation (not accounting for leap seconds)
    let days = days_from_civil(year, month, day);
    Some(days * 86400 * 1000 + hour * 3600 * 1000 + minute * 60 * 1000 + second * 1000)
}

/// Parse a Dataview-style date string into milliseconds since epoch.
#[must_use]
pub fn parse_date_like_string(s: &str) -> Option<i64> {
    let s = s.trim();
    parse_date_string(s).or_else(|| {
        if matches_iso_year_month(s) {
            parse_date_string(&format!("{s}-01"))
        } else {
            None
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedWikilink {
    pub path: String,
    pub display: Option<String>,
    pub embed: bool,
    pub subpath: Option<String>,
    pub link_type: &'static str,
}

pub(crate) fn parse_wikilink(value: &str) -> Option<ParsedWikilink> {
    let trimmed = value.trim();
    let (embed, inner) = if trimmed.starts_with("![[") && trimmed.ends_with("]]") {
        (true, &trimmed[3..trimmed.len() - 2])
    } else if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
        (false, &trimmed[2..trimmed.len() - 2])
    } else {
        return None;
    };

    let (target, display) = split_unescaped_pipe(inner);
    let (path, subpath, link_type) = if let Some((path, block_id)) = target.split_once("#^") {
        (
            path.trim().to_string(),
            Some(block_id.trim().to_string()),
            "block",
        )
    } else if let Some((path, heading)) = target.split_once('#') {
        (
            path.trim().to_string(),
            Some(heading.trim().to_string()),
            "header",
        )
    } else {
        (target.trim().to_string(), None, "file")
    };

    Some(ParsedWikilink {
        path,
        display,
        embed,
        subpath: subpath.filter(|value| !value.is_empty()),
        link_type,
    })
}

pub(crate) fn link_meta_value(value: &Value) -> Value {
    let Value::String(text) = value else {
        return Value::Null;
    };
    let Some(link) = parse_wikilink(text) else {
        return Value::Null;
    };

    let mut object = serde_json::Map::new();
    object.insert(
        "display".to_string(),
        link.display.map_or(Value::Null, Value::String),
    );
    object.insert("embed".to_string(), Value::Bool(link.embed));
    object.insert("path".to_string(), Value::String(link.path));
    object.insert(
        "subpath".to_string(),
        link.subpath.map_or(Value::Null, Value::String),
    );
    object.insert(
        "type".to_string(),
        Value::String(link.link_type.to_string()),
    );
    Value::Object(object)
}

/// Civil date to days since Unix epoch (algorithm from Howard Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn timestamp_from_civil(year: i64, month: i64, day: i64) -> i64 {
    days_from_civil(year, month, day) * 86_400_000
}

/// Parse a duration string like "1d", "2w", "3h", "30m", "10s" into milliseconds.
#[must_use]
pub fn parse_duration_string(s: &str) -> Option<i64> {
    let s = s.trim().trim_matches(|c: char| c == '\'' || c == '"');
    if s.is_empty() {
        return None;
    }

    let mut total_ms = 0.0_f64;
    let mut matched_any = false;
    let mut index = 0_usize;
    let bytes = s.as_bytes();

    while index < s.len() {
        while index < s.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b',') {
            index += 1;
        }
        if index >= s.len() {
            break;
        }

        let number_start = index;
        if matches!(bytes[index], b'+' | b'-') {
            index += 1;
        }
        while index < s.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        if number_start == index
            || (index == number_start + 1 && matches!(bytes[number_start], b'+' | b'-'))
        {
            return None;
        }
        let value = s[number_start..index].parse::<f64>().ok()?;

        while index < s.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        let unit_start = index;
        while index < s.len() && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        if unit_start == index {
            return None;
        }

        let multiplier = match s[unit_start..index].to_ascii_lowercase().as_str() {
            "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => 1.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1000.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0 * 1000.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0 * 1000.0,
            "d" | "day" | "days" => 86_400.0 * 1000.0,
            "w" | "wk" | "wks" | "week" | "weeks" => 7.0 * 86_400.0 * 1000.0,
            "mo" | "mos" | "month" | "months" => 30.0 * 86_400.0 * 1000.0,
            "y" | "yr" | "yrs" | "year" | "years" => 365.0 * 86_400.0 * 1000.0,
            _ => return None,
        };
        total_ms += value * multiplier;
        matched_any = true;
    }

    if !matched_any || total_ms == 0.0 {
        return None;
    }

    #[allow(clippy::cast_possible_truncation)]
    {
        Some(total_ms as i64)
    }
}

/// Extract date components from a millisecond timestamp.
#[must_use]
pub fn date_components(ms: i64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let total_seconds = ms / 1000;
    let day_seconds = ((total_seconds % 86400) + 86400) % 86400;
    let hour = day_seconds / 3600;
    let minute = (day_seconds % 3600) / 60;
    let second = day_seconds % 60;
    let millisecond = ms.rem_euclid(1000);

    let days = if ms >= 0 {
        ms / 86_400_000
    } else {
        (ms - 86_399_999) / 86_400_000
    };
    let (year, month, day) = civil_from_days(days);

    (year, month, day, hour, minute, second, millisecond)
}

#[must_use]
pub fn date_field_value(ms: i64, field: &str) -> Option<Value> {
    let (year, month, day, hour, minute, second, millisecond) = date_components(ms);
    let (weekyear, week) = iso_week_components(year, month, day);
    let weekday = iso_weekday(year, month, day);

    match field {
        "year" => Some(Value::Number(year.into())),
        "month" => Some(Value::Number(month.into())),
        "day" => Some(Value::Number(day.into())),
        "hour" => Some(Value::Number(hour.into())),
        "minute" => Some(Value::Number(minute.into())),
        "second" => Some(Value::Number(second.into())),
        "millisecond" => Some(Value::Number(millisecond.into())),
        "weekday" => Some(Value::Number(weekday.into())),
        "week" => Some(Value::Number(week.into())),
        "weekyear" => Some(Value::Number(weekyear.into())),
        _ => None,
    }
}

#[must_use]
pub fn file_field_type_name(field: &str) -> Option<&'static str> {
    match field {
        "path" | "name" | "basename" | "ext" | "folder" => Some("string"),
        "link" => Some("link"),
        "size" => Some("number"),
        "mtime" | "ctime" | "mday" | "cday" | "day" => Some("date"),
        "tags" | "etags" | "inlinks" | "outlinks" | "links" | "aliases" | "tasks" | "lists" => {
            Some("array")
        }
        "frontmatter" | "properties" => Some("object"),
        "starred" => Some("boolean"),
        _ => None,
    }
}

#[must_use]
pub fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(s) => {
            let trimmed = s.trim();
            if looks_like_wikilink(trimmed) {
                "link"
            } else if parse_date_like_string(trimmed).is_some() {
                "date"
            } else if parse_duration_string(trimmed).is_some() {
                "duration"
            } else {
                "string"
            }
        }
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_name_hint(expr: &Expr) -> Option<&'static str> {
    match expr {
        Expr::Null => Some("null"),
        Expr::Bool(_) => Some("boolean"),
        Expr::Number(_) => Some("number"),
        Expr::Str(text) => Some(value_type_name(&Value::String(text.clone()))),
        Expr::Array(_) => Some("array"),
        Expr::Object(_) => Some("object"),
        Expr::FunctionCall(name, _) => match name.as_str() {
            "now" | "today" | "date" => Some("date"),
            "duration" | "dur" => Some("duration"),
            "link" => Some("link"),
            "list" => Some("array"),
            "meta" => Some("object"),
            "typeof" => Some("string"),
            _ => None,
        },
        Expr::FieldAccess(receiver, field) if matches!(receiver.as_ref(), Expr::Identifier(name) if name == "file") => {
            file_field_type_name(field)
        }
        _ => None,
    }
}

fn date_shortcut_arg(expr: &Expr, ctx: &EvalContext) -> Option<i64> {
    let Expr::Identifier(name) = expr else {
        return None;
    };
    date_shortcut_value(name, ctx.now_ms)
}

fn date_shortcut_value(name: &str, now_ms: i64) -> Option<i64> {
    let today = start_of_day(now_ms);
    let day_ms = 86_400_000_i64;
    let (year, month, day, _, _, _, _) = date_components(today);

    match name {
        "today" => Some(today),
        "now" => Some(now_ms),
        "tomorrow" => Some(today + day_ms),
        "yesterday" => Some(today - day_ms),
        "sow" => {
            let weekday = iso_weekday(year, month, day);
            Some(today - (weekday - 1) * day_ms)
        }
        "eow" => {
            let weekday = iso_weekday(year, month, day);
            Some(today + (7 - weekday) * day_ms)
        }
        "som" => Some(timestamp_from_civil(year, month, 1)),
        "eom" => {
            let (next_year, next_month) = if month == 12 {
                (year + 1, 1)
            } else {
                (year, month + 1)
            };
            Some(timestamp_from_civil(next_year, next_month, 1) - day_ms)
        }
        "soy" => Some(timestamp_from_civil(year, 1, 1)),
        "eoy" => Some(timestamp_from_civil(year + 1, 1, 1) - day_ms),
        _ => None,
    }
}

fn start_of_day(ms: i64) -> i64 {
    ms.div_euclid(86_400_000) * 86_400_000
}

fn iso_weekday(year: i64, month: i64, day: i64) -> i64 {
    (days_from_civil(year, month, day) + 3).rem_euclid(7) + 1
}

fn iso_week_components(year: i64, month: i64, day: i64) -> (i64, i64) {
    let weekday = iso_weekday(year, month, day);
    let ordinal = ordinal_day(year, month, day);
    let mut week = (ordinal - weekday + 10).div_euclid(7);
    let mut weekyear = year;

    if week < 1 {
        weekyear -= 1;
        week = iso_weeks_in_year(weekyear);
    } else if week > iso_weeks_in_year(weekyear) {
        weekyear += 1;
        week = 1;
    }

    (weekyear, week)
}

fn ordinal_day(year: i64, month: i64, day: i64) -> i64 {
    days_from_civil(year, month, day) - days_from_civil(year, 1, 1) + 1
}

fn iso_weeks_in_year(year: i64) -> i64 {
    let jan1_weekday = iso_weekday(year, 1, 1);
    let dec31_weekday = iso_weekday(year, 12, 31);
    if jan1_weekday == 4 || dec31_weekday == 4 {
        53
    } else {
        52
    }
}

fn matches_iso_year_month(value: &str) -> bool {
    value.len() == 7
        && value.as_bytes()[4] == b'-'
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 4 || byte.is_ascii_digit())
}

fn looks_like_wikilink(value: &str) -> bool {
    parse_wikilink(value).is_some()
}

fn split_unescaped_pipe(link: &str) -> (String, Option<String>) {
    let bytes = link.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'|' && (index == 0 || bytes[index - 1] != b'\\') {
            return (
                link[..index].replace("\\|", "|"),
                Some(link[index + 1..].replace("\\|", "|")),
            );
        }
    }
    (link.replace("\\|", "|"), None)
}

/// Days since Unix epoch to civil date.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Format a date timestamp using a subset of Moment.js tokens.
#[must_use]
pub fn format_date(ms: i64, format: &str) -> String {
    let (year, month, day, hour, minute, second, millisecond) = date_components(ms);
    format
        .replace("YYYY", &format!("{year:04}"))
        .replace("MM", &format!("{month:02}"))
        .replace("DD", &format!("{day:02}"))
        .replace("HH", &format!("{hour:02}"))
        .replace("mm", &format!("{minute:02}"))
        .replace("ss", &format!("{second:02}"))
        .replace("SSS", &format!("{millisecond:03}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_date_string() {
        let ms = parse_date_string("2025-01-15").unwrap();
        let (y, m, d, _, _, _, _) = date_components(ms);
        assert_eq!((y, m, d), (2025, 1, 15));
    }

    #[test]
    fn test_parse_date_with_time() {
        let ms = parse_date_string("2025-06-15 14:30:00").unwrap();
        let (year, month, day, hour, minute, second, _) = date_components(ms);
        assert_eq!(
            (year, month, day, hour, minute, second),
            (2025, 6, 15, 14, 30, 0)
        );
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration_string("1d"), Some(86_400_000));
        assert_eq!(parse_duration_string("2h"), Some(7_200_000));
        assert_eq!(parse_duration_string("30m"), Some(1_800_000));
        assert_eq!(parse_duration_string("1w"), Some(604_800_000));
        assert_eq!(parse_duration_string("3 hours"), Some(10_800_000));
        assert_eq!(parse_duration_string("1d 3h 20m"), Some(98_400_000));
        assert_eq!(
            parse_duration_string("9 years, 8 months, 4 days, 16 hours, 2 minutes"),
            Some(304_963_320_000)
        );
        assert_eq!(parse_duration_string("2yr8mo12d"), Some(84_844_800_000));
    }

    #[test]
    fn test_parse_date_like_string() {
        let ms = parse_date_like_string("2026-04").unwrap();
        let (year, month, day, _, _, _, _) = date_components(ms);
        assert_eq!((year, month, day), (2026, 4, 1));
    }

    #[test]
    fn test_date_field_value() {
        let ms = parse_date_string("2026-01-01T12:34:56").unwrap();
        assert_eq!(
            date_field_value(ms, "year"),
            Some(Value::Number(2026.into()))
        );
        assert_eq!(date_field_value(ms, "week"), Some(Value::Number(1.into())));
        assert_eq!(
            date_field_value(ms, "weekyear"),
            Some(Value::Number(2026.into()))
        );
        assert_eq!(
            date_field_value(ms, "weekday"),
            Some(Value::Number(4.into()))
        );
    }

    #[test]
    fn test_format_date() {
        let ms = parse_date_string("2025-06-15 14:30:45").unwrap();
        assert_eq!(format_date(ms, "YYYY-MM-DD"), "2025-06-15");
        assert_eq!(
            format_date(ms, "YYYY-MM-DD HH:mm:ss"),
            "2025-06-15 14:30:45"
        );
    }

    #[test]
    fn test_parse_wikilink() {
        assert_eq!(
            parse_wikilink("![[My Project#^9bcbe8|Preview]]"),
            Some(ParsedWikilink {
                path: "My Project".to_string(),
                display: Some("Preview".to_string()),
                embed: true,
                subpath: Some("9bcbe8".to_string()),
                link_type: "block",
            })
        );
    }

    #[test]
    fn test_link_meta_value() {
        assert_eq!(
            link_meta_value(&Value::String("[[My Project#Next Actions]]".to_string())),
            serde_json::json!({
                "display": null,
                "embed": false,
                "path": "My Project",
                "subpath": "Next Actions",
                "type": "header",
            })
        );
    }

    #[test]
    fn test_default_value_vectorizes() {
        assert_eq!(
            default_value(
                serde_json::json!([1, null, null]),
                serde_json::json!(2),
                true
            ),
            serde_json::json!([1, 2, 2])
        );
        assert_eq!(
            default_value(
                serde_json::json!([null, 2]),
                serde_json::json!([3, 4]),
                true
            ),
            serde_json::json!([3, 2])
        );
    }

    #[test]
    fn test_days_round_trip() {
        // Verify civil date round-trips through days
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2025, 12, 31)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
    }
}
