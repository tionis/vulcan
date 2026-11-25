//! Paging and response detail shared by the MCP list tools: a `limit`, an
//! opaque `cursor`, and `page: {limit, returned, has_more, next_cursor}`.

use base64::Engine;
use serde_json::{Map, Value};

use crate::mcp_protocol::McpMethodError;

const CURSOR_PREFIX: &str = "v1:";

/// Encode the position after a page as an opaque cursor.
#[must_use]
pub fn encode_cursor(offset: usize) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{CURSOR_PREFIX}{offset}"))
}

/// The position a cursor names; no cursor starts at the beginning.
pub fn decode_cursor(tool: &str, cursor: Option<&str>) -> Result<usize, McpMethodError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| text.strip_prefix(CURSOR_PREFIX).map(str::to_string))
        .and_then(|offset| offset.parse::<usize>().ok())
        .ok_or_else(|| {
            McpMethodError::invalid_argument(
                format!(
                    "`{tool}.cursor` is not a cursor this tool returned; start again without it"
                ),
                Some("cursor".to_string()),
            )
        })
}

/// Check `limit` against a tool's maximum.
pub fn check_limit(tool: &str, limit: usize, max: usize) -> Result<usize, McpMethodError> {
    if limit == 0 || limit > max {
        return Err(McpMethodError::invalid_argument(
            format!("`{tool}.limit` must be between 1 and {max}"),
            Some("limit".to_string()),
        ));
    }
    Ok(limit)
}

/// The bounds of one page in a list of `total` items.
#[must_use]
pub fn page_bounds(start: usize, limit: usize, total: usize) -> (usize, usize) {
    let start = start.min(total);
    (start, start.saturating_add(limit).min(total))
}

/// The `page` object: `total` when the full count is known, otherwise
/// `has_more` comes from whether a further item was seen.
#[must_use]
pub fn page_json(
    limit: usize,
    start: usize,
    end: usize,
    total: Option<usize>,
    has_more: bool,
) -> Value {
    let mut page = serde_json::json!({
        "limit": limit,
        "returned": end.saturating_sub(start),
        "has_more": has_more,
        "next_cursor": has_more.then(|| encode_cursor(end)),
    });
    if let Some(total) = total {
        page["total"] = Value::from(total);
    }
    page
}

/// `concise` (the default) or `full`.
pub fn wants_full_detail(tool: &str, detail: Option<&str>) -> Result<bool, McpMethodError> {
    match detail.unwrap_or("concise") {
        "concise" => Ok(false),
        "full" => Ok(true),
        other => Err(McpMethodError::invalid_argument(
            format!("unsupported `{tool}.detail`: {other}; use `concise` or `full`"),
            Some("detail".to_string()),
        )),
    }
}

/// Keep only `fields` of an object, renamed to `snake_case` and dropping
/// nulls, empty arrays, and `false`: the concise form of a row.
#[must_use]
pub fn concise_row(row: &Value, fields: &[(&str, &str)]) -> Value {
    let mut object = Map::new();
    for (source, target) in fields {
        let Some(value) = row.get(*source) else {
            continue;
        };
        let empty = value.is_null()
            || value.as_array().is_some_and(Vec::is_empty)
            || value == &Value::Bool(false)
            || value.as_str().is_some_and(str::is_empty);
        if !empty {
            object.insert((*target).to_string(), value.clone());
        }
    }
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cursors_round_trip_and_reject_foreign_values() {
        let cursor = encode_cursor(40);
        assert_eq!(decode_cursor("search", Some(&cursor)).unwrap(), 40);
        assert_eq!(decode_cursor("search", None).unwrap(), 0);
        for foreign in ["40", "djE6eA", "!!"] {
            assert!(decode_cursor("search", Some(foreign)).is_err(), "{foreign}");
        }
    }

    #[test]
    fn pages_and_concise_rows_have_stable_shapes() {
        let page = page_json(10, 20, 30, Some(45), true);
        assert_eq!(page["returned"], 10);
        assert_eq!(page["total"], 45);
        assert_eq!(
            decode_cursor("t", page["next_cursor"].as_str()).unwrap(),
            30
        );
        assert!(page_json(10, 40, 45, None, false)["next_cursor"].is_null());
        assert_eq!(page_bounds(50, 10, 45), (45, 45));
        let row =
            json!({"statusName": "Todo", "tags": [], "due": null, "completed": false, "text": "x"});
        assert_eq!(
            concise_row(
                &row,
                &[
                    ("statusName", "status"),
                    ("tags", "tags"),
                    ("due", "due"),
                    ("completed", "completed"),
                    ("text", "text")
                ]
            ),
            json!({"status": "Todo", "text": "x"})
        );
        assert!(wants_full_detail("t", Some("full")).unwrap());
        assert!(wants_full_detail("t", Some("verbose")).is_err());
    }
}
