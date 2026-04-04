use crate::{Cli, CliError};
use serde::Serialize;
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListOutputControls {
    pub(crate) fields: Option<Vec<String>>,
    pub(crate) limit: Option<usize>,
    pub(crate) offset: usize,
}

impl ListOutputControls {
    pub(crate) fn from_cli(cli: &Cli) -> Self {
        Self {
            fields: cli.fields.clone(),
            limit: cli.limit,
            offset: cli.offset,
        }
    }

    pub(crate) fn with_saved_defaults(
        &self,
        fields: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> Self {
        Self {
            fields: self.fields.clone().or(fields),
            limit: self.limit.or(limit),
            offset: self.offset,
        }
    }

    pub(crate) fn requested_result_limit(&self) -> Option<usize> {
        self.limit.map(|limit| limit.saturating_add(self.offset))
    }
}

pub(crate) fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    println!(
        "{}",
        serde_json::to_string(value).map_err(CliError::operation)?
    );
    Ok(())
}

pub(crate) fn print_json_lines(
    rows: Vec<Value>,
    fields: Option<&[String]>,
) -> Result<(), CliError> {
    for row in rows {
        let selected = select_fields(row, fields);
        println!(
            "{}",
            serde_json::to_string(&selected).map_err(CliError::operation)?
        );
    }
    Ok(())
}

pub(crate) fn select_fields(row: Value, fields: Option<&[String]>) -> Value {
    let Some(fields) = fields else {
        return row;
    };
    let Some(object) = row.as_object() else {
        return row;
    };
    let mut selected = Map::new();
    for field in fields {
        if let Some(value) = object.get(field) {
            selected.insert(field.clone(), value.clone());
        }
    }
    Value::Object(selected)
}

pub(crate) fn print_selected_human_fields(row: &Value, fields: &[String]) {
    let Some(object) = row.as_object() else {
        println!("{row}");
        return;
    };

    let rendered = fields
        .iter()
        .filter_map(|field| {
            object
                .get(field)
                .map(|value| format!("{field}={}", render_human_value(value)))
        })
        .collect::<Vec<_>>();

    println!("{}", rendered.join(" | "));
}

#[allow(clippy::float_cmp, clippy::cast_possible_truncation)]
pub(crate) fn render_human_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f == f.trunc() && f.abs() < 1e15 {
                format!("{}", f as i64)
            } else {
                n.to_string()
            }
        }
        _ => value.to_string(),
    }
}

pub(crate) fn paginated_items<'a, T>(items: &'a [T], controls: &ListOutputControls) -> &'a [T] {
    let start = controls.offset.min(items.len());
    let end = controls.limit.map_or(items.len(), |limit| {
        start.saturating_add(limit).min(items.len())
    });

    &items[start..end]
}

pub(crate) fn render_dataview_inline_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => serde_json::to_string(value).expect("inline result should serialize"),
    }
}
