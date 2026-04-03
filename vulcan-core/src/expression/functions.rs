use std::fmt::Write;

use blake3::Hasher;
use regex::Regex;
use serde_json::Value;

use crate::expression::ast::{BinOp, Expr};
use crate::expression::eval::{
    as_number, compare_values, eval_binary_op, evaluate, is_truthy, number_to_value,
    resolve_note_reference, value_to_display, EvalContext,
};
use crate::expression::methods::{call_method, evaluate_callback};
use crate::expression::value::dataview_value_type;
use crate::file_metadata::FileMetadataResolver;

#[allow(clippy::too_many_lines)]
pub fn call_function(name: &str, args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    match name {
        "if" => func_if(args, ctx),
        "now" => Ok(Value::Number(localized_now_ms(ctx).into())),
        "today" => Ok(Value::Number(start_of_day(localized_now_ms(ctx)).into())),
        "date" => func_date(args, ctx),
        "meta" => func_meta(args, ctx),
        "typeof" => func_typeof(args, ctx),
        "length" => func_length(args, ctx),
        "object" => func_object(args, ctx),
        "extract" => func_extract(args, ctx),
        "round" => func_round(args, ctx),
        "trunc" => func_numeric_unary(args, ctx, f64::trunc),
        "floor" => func_numeric_unary(args, ctx, f64::floor),
        "ceil" => func_numeric_unary(args, ctx, f64::ceil),
        "default" => func_default(args, ctx, true),
        "ldefault" => func_default(args, ctx, false),
        "choice" => func_choice(args, ctx),
        "display" => func_display(args, ctx),
        "hash" => func_hash(args, ctx),
        "currencyformat" => func_currencyformat(args, ctx),
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
        "lower" => func_string_map(args, ctx, str::to_lowercase),
        "upper" => func_string_map(args, ctx, str::to_uppercase),
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
        "dateformat" => func_dateformat(args, ctx),
        "durationformat" => func_durationformat(args, ctx),
        "striptime" => func_striptime(args, ctx),
        "localtime" => func_localtime(args, ctx),
        "map" | "filter" | "sort" | "reverse" | "unique" | "flat" | "slice" => {
            func_array_alias(name, args, ctx)
        }
        "number" => func_number(args, ctx),
        "max" => func_extreme(args, ctx, true),
        "min" => func_extreme(args, ctx, false),
        "sum" => func_aggregate(args, ctx, "+"),
        "product" => func_aggregate(args, ctx, "*"),
        "average" => func_average(args, ctx),
        "reduce" => func_reduce(args, ctx),
        "minby" => func_extreme_by(args, ctx, false),
        "maxby" => func_extreme_by(args, ctx, true),
        "link" => func_link(args, ctx),
        "embed" => func_embed(args, ctx),
        "elink" => func_elink(args, ctx),
        "string" => func_string(args, ctx),
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

#[derive(Clone, Copy)]
enum RenderMode {
    String,
    Display,
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

    if args.len() > 1 {
        let text = eval_arg(args, 0, ctx)?;
        let format = eval_arg(args, 1, ctx)?;
        return Ok(match (&text, &format) {
            (Value::String(text), Value::String(format)) => parse_date_with_format(text, format)
                .map_or(Value::Null, |ms| Value::Number(ms.into())),
            _ => Value::Null,
        });
    }

    let val = eval_arg(args, 0, ctx)?;
    vectorize_unary(val, &|value| match &value {
        Value::String(s) => {
            Ok(resolve_date_value(s, ctx).map_or(Value::Null, |ms| Value::Number(ms.into())))
        }
        Value::Number(_) => Ok(value),
        _ => Ok(Value::Null),
    })
}

fn func_duration(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let val = eval_arg(args, 0, ctx)?;
    vectorize_unary(val, &|value| match &value {
        Value::String(s) => {
            Ok(parse_duration_string(s).map_or(Value::Null, |ms| Value::Number(ms.into())))
        }
        Value::Number(_) => Ok(value),
        _ => Ok(Value::Null),
    })
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

fn func_extract(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    match value {
        Value::Array(values) => Ok(Value::Array(
            values
                .into_iter()
                .map(|value| extract_keys(&value, args, ctx))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        value => extract_keys(&value, args, ctx),
    }
}

fn func_round(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let precision_value = eval_arg(args, 1, ctx)?;
    vectorize_binary(
        value,
        precision_value,
        true,
        false,
        &|value, precision_value| {
            let number = as_number(&value);
            if value.is_null() || number.is_nan() {
                return Ok(Value::Null);
            }

            let precision = as_number(&precision_value);
            if args.len() < 2 || precision_value.is_null() || precision.is_nan() || precision <= 0.0
            {
                return Ok(number_to_value(number.round()));
            }

            let factor = 10_f64.powf(precision.trunc());
            Ok(number_to_value((number * factor).round() / factor))
        },
    )
}

fn func_numeric_unary(
    args: &[Expr],
    ctx: &EvalContext,
    op: fn(f64) -> f64,
) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    vectorize_unary(value, &|value| {
        let number = as_number(&value);
        if value.is_null() || number.is_nan() {
            return Ok(Value::Null);
        }
        Ok(number_to_value(op(number)))
    })
}

fn func_number(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let val = eval_arg(args, 0, ctx)?;
    vectorize_unary(val, &|value| {
        Ok(match &value {
            Value::Number(_) => value,
            Value::String(text) => first_number_in_text(text).map_or(Value::Null, number_to_value),
            _ => Value::Null,
        })
    })
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
    let left = eval_arg(args, 1, ctx)?;
    let right = eval_arg(args, 2, ctx)?;
    vectorize_ternary(
        condition,
        left,
        right,
        true,
        false,
        false,
        &|condition, left, right| {
            if is_truthy(&condition) {
                Ok(left)
            } else {
                Ok(right)
            }
        },
    )
}

fn func_display(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let expr = args.first();
    let value = eval_arg(args, 0, ctx)?;
    Ok(Value::String(render_value(
        expr,
        &value,
        RenderMode::Display,
    )))
}

fn func_hash(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let values = eval_all_args(args, ctx)?;
    let mut hasher = Hasher::new();
    for value in &values {
        hasher.update(render_value(None, value, RenderMode::String).as_bytes());
        hasher.update(&[0]);
    }
    let bytes = hasher.finalize();
    let mut raw = [0_u8; 8];
    raw.copy_from_slice(&bytes.as_bytes()[..8]);
    let value = u64::from_le_bytes(raw) % 9_000_000_000_000_000;
    let value = i64::try_from(value).expect("bounded hash value should fit in i64");
    Ok(Value::Number(value.into()))
}

fn func_currencyformat(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let currency = match eval_arg(args, 1, ctx)? {
        Value::String(code) => code,
        Value::Null => "USD".to_string(),
        _ => return Ok(Value::Null),
    };
    vectorize_binary(
        value,
        Value::String(currency),
        true,
        false,
        &|value, currency| {
            let number = as_number(&value);
            let Value::String(currency) = currency else {
                return Ok(Value::Null);
            };
            if value.is_null() || number.is_nan() {
                return Ok(Value::Null);
            }
            Ok(Value::String(format_currency_value(number, &currency)))
        },
    )
}

fn func_contains(args: &[Expr], ctx: &EvalContext, mode: ContainsMode) -> Result<Value, String> {
    let haystack = eval_arg(args, 0, ctx)?;
    let needle = eval_arg(args, 1, ctx)?;
    vectorize_binary(haystack, needle, false, true, &|haystack, needle| {
        Ok(Value::Bool(contains_value(&haystack, &needle, mode)))
    })
}

fn func_contains_word(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let haystack = eval_arg(args, 0, ctx)?;
    let needle = eval_arg(args, 1, ctx)?;
    vectorize_binary(haystack, needle, true, true, &|haystack, needle| {
        Ok(contains_word_value(&haystack, &needle))
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
    vectorize_binary(haystack, needle, true, true, &|haystack, needle| {
        Ok(match (&haystack, &needle) {
            (Value::String(haystack), Value::String(needle)) => {
                Value::Bool(predicate(haystack, needle))
            }
            _ => Value::Null,
        })
    })
}

fn func_substring(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let start = eval_arg(args, 1, ctx)?;
    if args.len() > 2 {
        let end = eval_arg(args, 2, ctx)?;
        vectorize_ternary(
            value,
            start,
            end,
            true,
            true,
            true,
            &|value, start, end| match value {
                Value::String(text) => {
                    let Some(start) = nonnegative_index_value(&start) else {
                        return Ok(Value::Null);
                    };
                    let Some(end) = nonnegative_index_value(&end) else {
                        return Ok(Value::Null);
                    };
                    Ok(Value::String(substring_value(&text, start, Some(end))))
                }
                _ => Ok(Value::Null),
            },
        )
    } else {
        vectorize_binary(value, start, true, true, &|value, start| match value {
            Value::String(text) => {
                let Some(start) = nonnegative_index_value(&start) else {
                    return Ok(Value::Null);
                };
                Ok(Value::String(substring_value(&text, start, None)))
            }
            _ => Ok(Value::Null),
        })
    }
}

fn func_split(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let delimiter = eval_arg(args, 1, ctx)?;
    if args.len() > 2 {
        let limit = eval_arg(args, 2, ctx)?;
        vectorize_ternary(
            value,
            delimiter,
            limit,
            true,
            true,
            true,
            &|value, delimiter, limit| match (&value, &delimiter) {
                (Value::String(text), Value::String(pattern)) => {
                    let Some(limit) = nonnegative_index_value(&limit) else {
                        return Ok(Value::Null);
                    };
                    Ok(Value::Array(regex_split(text, pattern, Some(limit))?))
                }
                _ => Ok(Value::Null),
            },
        )
    } else {
        vectorize_binary(value, delimiter, true, true, &|value, delimiter| match (
            &value, &delimiter,
        ) {
            (Value::String(text), Value::String(pattern)) => {
                Ok(Value::Array(regex_split(text, pattern, None)?))
            }
            _ => Ok(Value::Null),
        })
    }
}

fn func_replace(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let pattern = eval_arg(args, 1, ctx)?;
    let replacement = eval_arg(args, 2, ctx)?;
    vectorize_ternary(
        value,
        pattern,
        replacement,
        true,
        true,
        true,
        &|value, pattern, replacement| match (&value, &pattern, &replacement) {
            (Value::String(text), Value::String(pattern), Value::String(replacement)) => {
                Ok(Value::String(text.replace(pattern, replacement)))
            }
            _ => Ok(Value::Null),
        },
    )
}

fn func_regextest(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let pattern = eval_arg(args, 0, ctx)?;
    let value = eval_arg(args, 1, ctx)?;
    vectorize_binary(pattern, value, true, true, &|pattern, value| {
        Ok(Value::Bool(match (&pattern, &value) {
            (Value::String(pattern), Value::String(text)) => {
                compile_regex(pattern, "regextest")?.is_match(text)
            }
            _ => false,
        }))
    })
}

fn func_regexmatch(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let pattern = eval_arg(args, 0, ctx)?;
    let value = eval_arg(args, 1, ctx)?;
    vectorize_binary(pattern, value, true, true, &|pattern, value| {
        Ok(Value::Bool(match (&pattern, &value) {
            (Value::String(pattern), Value::String(text)) => {
                compile_regex(&anchored_pattern(pattern), "regexmatch")?.is_match(text)
            }
            _ => false,
        }))
    })
}

fn func_regexreplace(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let pattern = eval_arg(args, 1, ctx)?;
    let replacement = eval_arg(args, 2, ctx)?;
    vectorize_ternary(
        value,
        pattern,
        replacement,
        true,
        true,
        true,
        &|value, pattern, replacement| match (&value, &pattern, &replacement) {
            (Value::String(text), Value::String(pattern), Value::String(replacement)) => {
                let regex = compile_regex(pattern, "regexreplace")?;
                Ok(Value::String(
                    regex.replace_all(text, replacement).into_owned(),
                ))
            }
            _ => Ok(Value::Null),
        },
    )
}

fn func_truncate(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let length = eval_arg(args, 1, ctx)?;
    if args.len() > 2 {
        let suffix = eval_arg(args, 2, ctx)?;
        vectorize_ternary(
            value,
            length,
            suffix,
            true,
            true,
            true,
            &|value, length, suffix| match (value, suffix) {
                (Value::String(text), Value::String(suffix)) => {
                    let Some(length) = nonnegative_index_value(&length) else {
                        return Ok(Value::Null);
                    };
                    Ok(Value::String(truncate_value(&text, length, &suffix)))
                }
                _ => Ok(Value::Null),
            },
        )
    } else {
        vectorize_binary(value, length, true, true, &|value, length| match value {
            Value::String(text) => {
                let Some(length) = nonnegative_index_value(&length) else {
                    return Ok(Value::Null);
                };
                Ok(Value::String(truncate_value(&text, length, "...")))
            }
            _ => Ok(Value::Null),
        })
    }
}

fn func_pad(args: &[Expr], ctx: &EvalContext, left: bool) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let target_length = eval_arg(args, 1, ctx)?;
    if args.len() > 2 {
        let padding = eval_arg(args, 2, ctx)?;
        vectorize_ternary(
            value,
            target_length,
            padding,
            true,
            true,
            true,
            &|value, target_length, padding| match (value, padding) {
                (Value::String(text), Value::String(padding)) if !padding.is_empty() => {
                    let Some(target_length) = nonnegative_index_value(&target_length) else {
                        return Ok(Value::Null);
                    };
                    Ok(Value::String(pad_string(
                        &text,
                        target_length,
                        &padding,
                        left,
                    )))
                }
                _ => Ok(Value::Null),
            },
        )
    } else {
        vectorize_binary(
            value,
            target_length,
            true,
            true,
            &|value, target_length| match value {
                Value::String(text) => {
                    let Some(target_length) = nonnegative_index_value(&target_length) else {
                        return Ok(Value::Null);
                    };
                    Ok(Value::String(pad_string(&text, target_length, " ", left)))
                }
                _ => Ok(Value::Null),
            },
        )
    }
}

fn func_dateformat(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let format = eval_arg(args, 1, ctx)?;
    vectorize_binary(value, format, true, false, &|value, format| {
        let Value::String(format) = format else {
            return Ok(Value::Null);
        };

        match date_value_ms(&value) {
            Some(ms) => Ok(Value::String(format_date(ms, &format))),
            None => Ok(Value::Null),
        }
    })
}

fn func_durationformat(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    let format = eval_arg(args, 1, ctx)?;
    vectorize_binary(value, format, true, false, &|value, format| {
        let Value::String(format) = format else {
            return Ok(Value::Null);
        };

        match duration_value_ms(&value) {
            Some(ms) => Ok(Value::String(format_duration(ms, &format))),
            None if value.is_null() => Ok(Value::Null),
            None => Ok(Value::Null),
        }
    })
}

fn func_striptime(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    vectorize_unary(value, &|value| {
        Ok(match date_value_ms(&value) {
            Some(ms) => Value::Number(start_of_day(ms).into()),
            None => Value::Null,
        })
    })
}

fn func_localtime(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    vectorize_unary(value, &|value| {
        Ok(match date_value_ms(&value) {
            Some(ms) => Value::Number(ctx.time_zone.localize_utc_ms(ms).into()),
            None => Value::Null,
        })
    })
}

fn func_embed(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let value = eval_arg(args, 0, ctx)?;
    if args.len() > 1 {
        let should_embed = eval_arg(args, 1, ctx)?;
        vectorize_binary(value, should_embed, true, true, &|value, should_embed| {
            let Value::String(text) = value else {
                return Ok(Value::Null);
            };

            let Some(link) = parse_wikilink(&text) else {
                return Ok(Value::Null);
            };
            let Value::Bool(should_embed) = should_embed else {
                return Ok(Value::Null);
            };

            Ok(Value::String(rebuild_link(&link, should_embed)))
        })
    } else {
        vectorize_unary(value, &|value| {
            let Value::String(text) = value else {
                return Ok(Value::Null);
            };

            let Some(link) = parse_wikilink(&text) else {
                return Ok(Value::Null);
            };
            Ok(Value::String(rebuild_link(&link, true)))
        })
    }
}

fn func_elink(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let url = eval_arg(args, 0, ctx)?;
    if args.len() > 1 {
        let display = eval_arg(args, 1, ctx)?;
        vectorize_binary(url, display, true, true, &|url, display| {
            let Value::String(url) = url else {
                return Ok(Value::Null);
            };
            let display = match display {
                Value::String(display) => display,
                Value::Null => url.clone(),
                _ => return Ok(Value::Null),
            };

            Ok(Value::String(format!("[{display}]({url})")))
        })
    } else {
        vectorize_unary(url, &|url| {
            let Value::String(url) = url else {
                return Ok(Value::Null);
            };
            Ok(Value::String(format!("[{url}]({url})")))
        })
    }
}

fn func_link(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let path = eval_arg(args, 0, ctx)?;
    if args.len() > 1 {
        let display = eval_arg(args, 1, ctx)?;
        vectorize_binary(path, display, true, true, &|path, display| {
            if path.is_null() {
                return Ok(Value::Null);
            }

            let path_str = value_to_display(&path);
            if display.is_null() {
                Ok(Value::String(format!("[[{path_str}]]")))
            } else {
                Ok(Value::String(format!(
                    "[[{path_str}|{}]]",
                    value_to_display(&display)
                )))
            }
        })
    } else {
        vectorize_unary(path, &|path| {
            if path.is_null() {
                return Ok(Value::Null);
            }
            let path_str = value_to_display(&path);
            Ok(Value::String(format!("[[{path_str}]]")))
        })
    }
}

fn func_string(args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let expr = args.first();
    let value = eval_arg(args, 0, ctx)?;
    Ok(Value::String(render_value(
        expr,
        &value,
        RenderMode::String,
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

fn vectorize_unary<F>(value: Value, op: &F) -> Result<Value, String>
where
    F: Fn(Value) -> Result<Value, String>,
{
    match value {
        Value::Array(values) => Ok(Value::Array(
            values
                .into_iter()
                .map(|value| vectorize_unary(value, op))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        value => op(value),
    }
}

fn vectorize_binary<F>(
    left: Value,
    right: Value,
    vectorize_left: bool,
    vectorize_right: bool,
    op: &F,
) -> Result<Value, String>
where
    F: Fn(Value, Value) -> Result<Value, String>,
{
    match (left, right) {
        (Value::Array(left_values), Value::Array(right_values))
            if vectorize_left && vectorize_right =>
        {
            let len = left_values.len().min(right_values.len());
            Ok(Value::Array(
                (0..len)
                    .map(|index| {
                        vectorize_binary(
                            left_values[index].clone(),
                            right_values[index].clone(),
                            vectorize_left,
                            vectorize_right,
                            op,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
        (Value::Array(values), right) if vectorize_left => Ok(Value::Array(
            values
                .into_iter()
                .map(|value| {
                    vectorize_binary(value, right.clone(), vectorize_left, vectorize_right, op)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )),
        (left, Value::Array(values)) if vectorize_right => Ok(Value::Array(
            values
                .into_iter()
                .map(|value| {
                    vectorize_binary(left.clone(), value, vectorize_left, vectorize_right, op)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )),
        (left, right) => op(left, right),
    }
}

fn vectorize_ternary<F>(
    first: Value,
    second: Value,
    third: Value,
    vectorize_first: bool,
    vectorize_second: bool,
    vectorize_third: bool,
    op: &F,
) -> Result<Value, String>
where
    F: Fn(Value, Value, Value) -> Result<Value, String>,
{
    let first_is_array = vectorize_first && matches!(first, Value::Array(_));
    let second_is_array = vectorize_second && matches!(second, Value::Array(_));
    let third_is_array = vectorize_third && matches!(third, Value::Array(_));

    if first_is_array || second_is_array || third_is_array {
        let len = [
            (&first, vectorize_first),
            (&second, vectorize_second),
            (&third, vectorize_third),
        ]
        .into_iter()
        .filter_map(|(value, vectorize)| match (value, vectorize) {
            (Value::Array(values), true) => Some(values.len()),
            _ => None,
        })
        .min()
        .unwrap_or(0);

        return Ok(Value::Array(
            (0..len)
                .map(|index| {
                    let first_value = match &first {
                        Value::Array(values) if vectorize_first => values[index].clone(),
                        _ => first.clone(),
                    };
                    let second_value = match &second {
                        Value::Array(values) if vectorize_second => values[index].clone(),
                        _ => second.clone(),
                    };
                    let third_value = match &third {
                        Value::Array(values) if vectorize_third => values[index].clone(),
                        _ => third.clone(),
                    };
                    vectorize_ternary(
                        first_value,
                        second_value,
                        third_value,
                        vectorize_first,
                        vectorize_second,
                        vectorize_third,
                        op,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        ));
    }

    op(first, second, third)
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

fn extract_keys(value: &Value, args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    let mut result = serde_json::Map::new();
    for key_expr in &args[1..] {
        let key = evaluate(key_expr, ctx)?;
        let Value::String(key) = key else {
            return Err("extract(object, key1, ...) must be called with string keys".to_string());
        };
        let extracted = match &value {
            Value::Object(map) => map.get(&key).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
        result.insert(key, extracted);
    }
    Ok(Value::Object(result))
}

fn resolve_date_value(value: &str, ctx: &EvalContext) -> Option<i64> {
    parse_date_like_string(value).or_else(|| date_from_link(value, ctx))
}

fn date_from_link(value: &str, ctx: &EvalContext) -> Option<i64> {
    let link = parse_wikilink(value)?;
    if let Some(display) = link.display.as_deref().and_then(parse_date_like_string) {
        return Some(display);
    }
    if let Some(path) = parse_date_like_string(&link.path) {
        return Some(path);
    }

    let lookup = ctx.note_lookup?;
    let note = resolve_note_reference(lookup, &ctx.note.document_path, &link.path)?;
    match FileMetadataResolver::field(note, "day") {
        Value::String(day) => parse_date_like_string(&day),
        Value::Number(ms) => ms.as_i64(),
        _ => None,
    }
}

fn first_number_in_text(text: &str) -> Option<f64> {
    Regex::new(r"-?[0-9]+(\.[0-9]+)?")
        .ok()?
        .find(text)
        .and_then(|m| m.as_str().parse::<f64>().ok())
}

fn date_value_ms(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => parse_date_like_string(text),
        _ => None,
    }
}

fn duration_value_ms(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => parse_duration_string(text),
        _ => None,
    }
}

fn render_value(expr: Option<&Expr>, value: &Value, mode: RenderMode) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) | Value::Object(_) => value_to_display(value),
        Value::Number(number) => {
            let hint = expr.and_then(type_name_hint);
            if hint == Some("date") {
                return pretty_date(number.as_i64().unwrap_or(0));
            }
            if hint == Some("duration") {
                return human_duration(number.as_i64().unwrap_or(0));
            }
            value_to_display(value)
        }
        Value::String(text) => {
            let hint = expr.and_then(type_name_hint);
            if hint == Some("date") {
                if let Some(ms) = parse_date_like_string(text) {
                    return pretty_date(ms);
                }
            }
            if hint == Some("duration") {
                if let Some(ms) = parse_duration_string(text) {
                    return human_duration(ms);
                }
            }
            if let Some(link) = parse_wikilink(text) {
                return match mode {
                    RenderMode::String => text.clone(),
                    RenderMode::Display => link_display_text(&link),
                };
            }
            match mode {
                RenderMode::String => text.clone(),
                RenderMode::Display => strip_markdown_display(text),
            }
        }
        Value::Array(values) => values
            .iter()
            .map(|value| render_value(None, value, mode))
            .collect::<Vec<_>>()
            .join(", "),
    }
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
        _ => Value::Null,
    }
}

fn nonnegative_index_value(value: &Value) -> Option<usize> {
    let number = as_number(value);
    if value.is_null() || number.is_nan() {
        None
    } else {
        format!("{:.0}", number.max(0.0).trunc())
            .parse::<usize>()
            .ok()
    }
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

fn link_display_text(link: &ParsedWikilink) -> String {
    link.display.clone().unwrap_or_else(|| {
        let base = link.path.rsplit('/').next().unwrap_or(&link.path);
        base.trim_end_matches(".md").to_string()
    })
}

fn strip_markdown_display(text: &str) -> String {
    let markdown_link = Regex::new(r"\[([^\]]+)\]\([^)]+\)").expect("valid markdown-link regex");
    let wikilink = Regex::new(r"!?(\[\[[^\]]+\]\])").expect("valid wikilink regex");

    let text = markdown_link.replace_all(text, "$1");
    let text = wikilink.replace_all(&text, |captures: &regex::Captures<'_>| {
        captures
            .get(1)
            .and_then(|m| parse_wikilink(m.as_str()))
            .map_or_else(String::new, |link| link_display_text(&link))
    });

    text.replace("**", "")
        .replace("__", "")
        .replace("~~", "")
        .replace("==", "")
        .replace(['`', '*', '_'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn pretty_date(ms: i64) -> String {
    format_date(ms, "MMMM d, yyyy")
}

fn human_duration(ms: i64) -> String {
    let mut remaining = ms.abs();
    let sign = if ms < 0 { "-" } else { "" };
    let mut parts = Vec::new();

    for (label, unit_ms) in [
        ("year", 365 * 86_400_000_i64),
        ("month", 30 * 86_400_000_i64),
        ("week", 7 * 86_400_000_i64),
        ("day", 86_400_000_i64),
        ("hour", 3_600_000_i64),
        ("minute", 60_000_i64),
        ("second", 1_000_i64),
        ("millisecond", 1_i64),
    ] {
        let count = remaining / unit_ms;
        if count > 0 {
            parts.push(format!(
                "{count} {label}{}",
                if count == 1 { "" } else { "s" }
            ));
            remaining %= unit_ms;
        }
    }

    if parts.is_empty() {
        "0 milliseconds".to_string()
    } else {
        format!("{sign}{}", parts.join(" "))
    }
}

fn rebuild_link(link: &ParsedWikilink, embed: bool) -> String {
    let mut target = link.path.clone();
    if let Some(subpath) = &link.subpath {
        target.push('#');
        if link.link_type == "block" {
            target.push('^');
        }
        target.push_str(subpath);
    }
    let display = link
        .display
        .as_ref()
        .map_or_else(String::new, |display| format!("|{display}"));
    let prefix = if embed { "!" } else { "" };
    format!("{prefix}[[{target}{display}]]")
}

fn format_currency_value(number: f64, currency: &str) -> String {
    let sign = if number < 0.0 { "-" } else { "" };
    let rounded = format!("{:.2}", number.abs());
    let (whole, fractional) = rounded.split_once('.').unwrap_or((&rounded, "00"));
    format!("{sign}{currency} {}.{fractional}", comma_separate(whole))
}

fn comma_separate(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let mut result = String::new();
    for (index, ch) in chars.iter().enumerate() {
        if index > 0 && (chars.len() - index) % 3 == 0 {
            result.push(',');
        }
        result.push(*ch);
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

    let (hour, minute, second, offset_ms) = if let Some(t) = time_part {
        let (time_text, offset_ms) = parse_time_zone_suffix(t)?;
        let mut tparts = time_text.split(':');
        let h: i64 = tparts
            .next()
            .and_then(|part| part.parse().ok())
            .unwrap_or(0);
        let m: i64 = tparts
            .next()
            .and_then(|part| part.parse().ok())
            .unwrap_or(0);
        let sec: i64 = tparts
            .next()
            .and_then(|part| part.split('.').next()?.parse().ok())
            .unwrap_or(0);
        (h, m, sec, offset_ms)
    } else {
        (0, 0, 0, 0)
    };

    // Simplified days-since-epoch calculation (not accounting for leap seconds)
    let days = days_from_civil(year, month, day);
    Some(days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1_000 - offset_ms)
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

#[must_use]
pub fn parse_date_with_format(text: &str, format: &str) -> Option<i64> {
    if format == "x" || format == "X" {
        let value = format!("{:.0}", first_number_in_text(text)?.trunc())
            .parse::<i64>()
            .ok()?;
        return Some(if format == "X" { value * 1000 } else { value });
    }

    let text = text.trim();
    let mut text_index = 0_usize;
    let mut format_index = 0_usize;
    let format_bytes = format.as_bytes();
    let text_bytes = text.as_bytes();

    let mut year = None;
    let mut month = None;
    let mut day = None;
    let mut hour = Some(0_i64);
    let mut minute = Some(0_i64);
    let mut second = Some(0_i64);

    while format_index < format.len() {
        if let Some((token, width)) = date_format_token(&format[format_index..]) {
            if text_index + width > text.len() {
                return None;
            }
            let segment = &text[text_index..text_index + width];
            if !segment.chars().all(|ch| ch.is_ascii_digit()) {
                return None;
            }
            let value = segment.parse::<i64>().ok()?;
            match token {
                "yyyy" => year = Some(value),
                "yy" => year = Some(2000 + value),
                "MM" => month = Some(value),
                "dd" => day = Some(value),
                "HH" => hour = Some(value),
                "mm" => minute = Some(value),
                "ss" => second = Some(value),
                _ => {}
            }
            format_index += token.len();
            text_index += width;
        } else {
            if text_bytes.get(text_index) != format_bytes.get(format_index) {
                return None;
            }
            format_index += 1;
            text_index += 1;
        }
    }

    if text_index != text.len() {
        return None;
    }

    let date = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year?,
        month?,
        day?,
        hour.unwrap_or(0),
        minute.unwrap_or(0),
        second.unwrap_or(0)
    );
    parse_date_string(&date)
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
    dataview_value_type(value).typeof_name()
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
    date_shortcut_value(name, localized_now_ms(ctx))
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

fn localized_now_ms(ctx: &EvalContext) -> i64 {
    ctx.time_zone.localize_utc_ms(ctx.now_ms)
}

fn start_of_day(ms: i64) -> i64 {
    ms.div_euclid(86_400_000) * 86_400_000
}

fn parse_time_zone_suffix(time_text: &str) -> Option<(&str, i64)> {
    let trimmed = time_text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.ends_with('Z') || trimmed.ends_with('z') {
        return Some((&trimmed[..trimmed.len() - 1], 0));
    }

    let bytes = trimmed.as_bytes();
    for (index, byte) in bytes.iter().enumerate().rev() {
        if !matches!(byte, b'+' | b'-') || index == 0 {
            continue;
        }
        let suffix = &trimmed[index..];
        let offset_ms = parse_time_zone_offset(suffix)?;
        return Some((&trimmed[..index], offset_ms));
    }

    Some((trimmed, 0))
}

fn parse_time_zone_offset(raw: &str) -> Option<i64> {
    let sign = match raw.as_bytes().first().copied() {
        Some(b'+') => 1_i64,
        Some(b'-') => -1_i64,
        _ => return None,
    };
    let rest = &raw[1..];
    let (hours, minutes) = if let Some((hours, minutes)) = rest.split_once(':') {
        (hours, minutes)
    } else if rest.len() == 4 {
        (&rest[..2], &rest[2..])
    } else if rest.len() == 2 {
        (rest, "00")
    } else {
        return None;
    };

    let hours = hours.parse::<i64>().ok()?;
    let minutes = minutes.parse::<i64>().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }

    Some(sign * (hours * 3_600_000 + minutes * 60_000))
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

fn date_format_token(format: &str) -> Option<(&'static str, usize)> {
    for token in [
        "yyyy", "MMMM", "MMM", "ffff", "SSS", "YYYY", "yy", "MM", "dd", "DD", "HH", "mm", "ss",
        "YY", "X", "x", "d", "D",
    ] {
        if format.starts_with(token) {
            let width = match token {
                "yyyy" | "YYYY" => 4,
                "yy" | "YY" | "MM" | "dd" | "DD" | "HH" | "mm" | "ss" => 2,
                "d" | "D" | "x" | "X" => 1,
                _ => token.len(),
            };
            return Some((token, width));
        }
    }
    None
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
    let mut rendered = String::new();
    let mut index = 0_usize;
    while index < format.len() {
        let remainder = &format[index..];
        if let Some((token, _)) = date_format_token(remainder) {
            match token {
                "yyyy" | "YYYY" => {
                    let _ = write!(rendered, "{year:04}");
                }
                "yy" | "YY" => {
                    let _ = write!(rendered, "{:02}", year.rem_euclid(100));
                }
                "MMMM" => rendered.push_str(month_name(month)),
                "MMM" => rendered.push_str(&month_name(month)[..3]),
                "MM" => {
                    let _ = write!(rendered, "{month:02}");
                }
                "dd" | "DD" => {
                    let _ = write!(rendered, "{day:02}");
                }
                "d" | "D" => {
                    let _ = write!(rendered, "{day}");
                }
                "HH" => {
                    let _ = write!(rendered, "{hour:02}");
                }
                "mm" => {
                    let _ = write!(rendered, "{minute:02}");
                }
                "ss" => {
                    let _ = write!(rendered, "{second:02}");
                }
                "SSS" => {
                    let _ = write!(rendered, "{millisecond:03}");
                }
                "x" => {
                    let _ = write!(rendered, "{ms}");
                }
                "X" => {
                    let _ = write!(rendered, "{}", ms / 1000);
                }
                "ffff" => {
                    let _ = write!(
                        rendered,
                        "{}, {} {}, {} {:02}:{:02}:{:02}",
                        weekday_name(iso_weekday(year, month, day)),
                        month_name(month),
                        day,
                        year,
                        hour,
                        minute,
                        second
                    );
                }
                _ => rendered.push_str(token),
            }
            index += token.len();
        } else if let Some(ch) = remainder.chars().next() {
            rendered.push(ch);
            index += ch.len_utf8();
        } else {
            break;
        }
    }
    rendered
}

#[must_use]
pub fn format_duration(ms: i64, format: &str) -> String {
    let mut remainder = ms.abs();
    let units = [
        ('y', 365 * 86_400_000_i64),
        ('M', 30 * 86_400_000_i64),
        ('w', 7 * 86_400_000_i64),
        ('d', 86_400_000_i64),
        ('h', 3_600_000_i64),
        ('m', 60_000_i64),
        ('s', 1_000_i64),
        ('S', 1_i64),
    ];
    let present = duration_tokens_in_format(format);

    let mut values = std::collections::BTreeMap::new();
    for (token, unit_ms) in units {
        if !present.contains(&token) {
            continue;
        }
        let value = remainder / unit_ms;
        values.insert(token, value);
        remainder %= unit_ms;
    }

    let mut rendered = String::new();
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            for literal in chars.by_ref() {
                if literal == '\'' {
                    break;
                }
                rendered.push(literal);
            }
            continue;
        }

        if values.contains_key(&ch) {
            let mut width = 1_usize;
            while chars.peek().is_some_and(|next| *next == ch) {
                chars.next();
                width += 1;
            }
            let value = values.get(&ch).copied().unwrap_or(0);
            let _ = write!(rendered, "{value:0width$}");
        } else {
            rendered.push(ch);
        }
    }
    rendered
}

fn duration_tokens_in_format(format: &str) -> Vec<char> {
    let mut tokens = Vec::new();
    let mut in_literal = false;
    for ch in format.chars() {
        if ch == '\'' {
            in_literal = !in_literal;
            continue;
        }
        if !in_literal
            && matches!(ch, 'y' | 'M' | 'w' | 'd' | 'h' | 'm' | 's' | 'S')
            && !tokens.contains(&ch)
        {
            tokens.push(ch);
        }
    }
    tokens
}

fn month_name(month: i64) -> &'static str {
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    usize::try_from(month.saturating_sub(1))
        .ok()
        .and_then(|index| MONTHS.get(index).copied())
        .unwrap_or("")
}

fn weekday_name(weekday: i64) -> &'static str {
    match weekday {
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        7 => "Sunday",
        _ => "",
    }
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
    fn test_parse_date_with_time_zone_offset() {
        let utc = parse_date_string("2025-06-15T12:30:00Z").unwrap();
        let shifted = parse_date_string("2025-06-15T14:30:00+02:00").unwrap();
        let compact = parse_date_string("2025-06-15T07:00:00-0530").unwrap();
        assert_eq!(utc, shifted);
        assert_eq!(compact, parse_date_string("2025-06-15T12:30:00Z").unwrap());
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
