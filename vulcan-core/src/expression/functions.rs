use serde_json::Value;

use crate::expression::ast::Expr;
use crate::expression::eval::{as_number, evaluate, is_truthy, number_to_value, value_to_display, EvalContext};

pub fn call_function(name: &str, args: &[Expr], ctx: &EvalContext) -> Result<Value, String> {
    match name {
        "if" => func_if(args, ctx),
        "now" => Ok(Value::Number(ctx.now_ms.into())),
        "today" => {
            // Truncate to start of day (UTC)
            let day_ms = 86_400_000_i64;
            let today = (ctx.now_ms / day_ms) * day_ms;
            Ok(Value::Number(today.into()))
        }
        "date" => {
            let val = eval_arg(args, 0, ctx)?;
            match &val {
                Value::String(s) => Ok(parse_date_string(s)
                    .map(|ms| Value::Number(ms.into()))
                    .unwrap_or(Value::Null)),
                Value::Number(_) => Ok(val),
                _ => Ok(Value::Null),
            }
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
        "max" => {
            let values = eval_all_args(args, ctx)?;
            let result = values
                .iter()
                .filter_map(|v| v.as_f64())
                .fold(f64::NEG_INFINITY, f64::max);
            if result == f64::NEG_INFINITY {
                Ok(Value::Null)
            } else {
                Ok(number_to_value(result))
            }
        }
        "min" => {
            let values = eval_all_args(args, ctx)?;
            let result = values
                .iter()
                .filter_map(|v| v.as_f64())
                .fold(f64::INFINITY, f64::min);
            if result == f64::INFINITY {
                Ok(Value::Null)
            } else {
                Ok(number_to_value(result))
            }
        }
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
            let val = eval_arg(args, 0, ctx)?;
            match val {
                Value::Array(_) => Ok(val),
                _ => Ok(Value::Array(vec![val])),
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
        "html" => {
            let val = eval_arg(args, 0, ctx)?;
            Ok(Value::String(value_to_display(&val)))
        }
        "image" => {
            let val = eval_arg(args, 0, ctx)?;
            Ok(Value::String(value_to_display(&val)))
        }
        "icon" => {
            let val = eval_arg(args, 0, ctx)?;
            Ok(Value::String(value_to_display(&val)))
        }
        "file" => {
            // Returns the path as a string; full file-object support deferred
            let val = eval_arg(args, 0, ctx)?;
            Ok(Value::String(value_to_display(&val)))
        }
        "duration" => {
            let val = eval_arg(args, 0, ctx)?;
            match &val {
                Value::String(s) => Ok(parse_duration_string(s)
                    .map(|ms| Value::Number(ms.into()))
                    .unwrap_or(Value::Null)),
                _ => Ok(Value::Null),
            }
        }
        _ => Err(format!("unknown function `{name}`")),
    }
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

fn eval_arg(args: &[Expr], index: usize, ctx: &EvalContext) -> Result<Value, String> {
    args.get(index)
        .map(|e| evaluate(e, ctx))
        .unwrap_or(Ok(Value::Null))
}

fn eval_all_args(args: &[Expr], ctx: &EvalContext) -> Result<Vec<Value>, String> {
    args.iter().map(|e| evaluate(e, ctx)).collect()
}

/// Parse an ISO 8601 date string into milliseconds since epoch.
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

/// Civil date to days since Unix epoch (algorithm from Howard Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse a duration string like "1d", "2w", "3h", "30m", "10s" into milliseconds.
pub fn parse_duration_string(s: &str) -> Option<i64> {
    let s = s.trim().trim_matches(|c: char| c == '\'' || c == '"');
    if s.is_empty() {
        return None;
    }

    let mut total_ms: i64 = 0;
    let mut num_str = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() || ch == '.' {
            num_str.push(ch);
        } else {
            let n: f64 = num_str.parse().ok()?;
            num_str.clear();
            let ms = match ch {
                's' => n * 1000.0,
                'm' => n * 60.0 * 1000.0,
                'h' => n * 3600.0 * 1000.0,
                'd' => n * 86400.0 * 1000.0,
                'w' => n * 7.0 * 86400.0 * 1000.0,
                'M' => n * 30.0 * 86400.0 * 1000.0,
                'y' => n * 365.0 * 86400.0 * 1000.0,
                _ => return None,
            };
            total_ms += ms as i64;
        }
    }

    if total_ms == 0 {
        return None;
    }

    Some(total_ms)
}

/// Extract date components from a millisecond timestamp.
pub fn date_components(ms: i64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let total_seconds = ms / 1000;
    let day_seconds = ((total_seconds % 86400) + 86400) % 86400;
    let hour = day_seconds / 3600;
    let minute = (day_seconds % 3600) / 60;
    let second = day_seconds % 60;
    let millisecond = ((ms % 1000) + 1000) % 1000;

    let days = if ms >= 0 {
        ms / 86_400_000
    } else {
        (ms - 86_399_999) / 86_400_000
    };
    let (year, month, day) = civil_from_days(days);

    (year, month, day, hour, minute, second, millisecond)
}

/// Days since Unix epoch to civil date.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Format a date timestamp using a subset of Moment.js tokens.
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
        let (y, m, d, h, min, s, _) = date_components(ms);
        assert_eq!((y, m, d, h, min, s), (2025, 6, 15, 14, 30, 0));
    }

    #[test]
    fn test_parse_duration() {
        assert_eq!(parse_duration_string("1d"), Some(86_400_000));
        assert_eq!(parse_duration_string("2h"), Some(7_200_000));
        assert_eq!(parse_duration_string("30m"), Some(1_800_000));
        assert_eq!(parse_duration_string("1w"), Some(604_800_000));
    }

    #[test]
    fn test_format_date() {
        let ms = parse_date_string("2025-06-15 14:30:45").unwrap();
        assert_eq!(format_date(ms, "YYYY-MM-DD"), "2025-06-15");
        assert_eq!(format_date(ms, "YYYY-MM-DD HH:mm:ss"), "2025-06-15 14:30:45");
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
