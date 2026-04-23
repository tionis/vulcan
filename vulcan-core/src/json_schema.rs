use serde_json::{Map, Value};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonSchemaValidationError {
    message: String,
}

impl JsonSchemaValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for JsonSchemaValidationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for JsonSchemaValidationError {}

pub fn validate_json_value_against_schema(
    value: &Value,
    schema: &Value,
) -> Result<(), JsonSchemaValidationError> {
    let schema_object = schema
        .as_object()
        .ok_or_else(|| JsonSchemaValidationError::new("JSON Schema roots must be objects"))?;
    validate_schema_node(value, schema_object, "$")
}

fn validate_schema_node(
    value: &Value,
    schema: &Map<String, Value>,
    path: &str,
) -> Result<(), JsonSchemaValidationError> {
    if let Some(any_of) = schema.get("anyOf") {
        validate_any_of(value, any_of, path)?;
    }
    if let Some(all_of) = schema.get("allOf") {
        validate_all_of(value, all_of, path)?;
    }
    if let Some(one_of) = schema.get("oneOf") {
        validate_one_of(value, one_of, path)?;
    }

    if let Some(type_keyword) = schema.get("type") {
        validate_type_keyword(value, type_keyword, path)?;
    }
    if let Some(const_keyword) = schema.get("const") {
        if value != const_keyword {
            return Err(JsonSchemaValidationError::new(format!(
                "{path} must equal {}",
                summarize_json_value(const_keyword)
            )));
        }
    }
    if let Some(enum_keyword) = schema.get("enum") {
        validate_enum_keyword(value, enum_keyword, path)?;
    }

    if let Some(minimum) = schema.get("minimum") {
        validate_numeric_bound(value, minimum, path, "minimum", |actual, minimum| {
            actual >= minimum
        })?;
    }
    if let Some(maximum) = schema.get("maximum") {
        validate_numeric_bound(value, maximum, path, "maximum", |actual, maximum| {
            actual <= maximum
        })?;
    }
    if let Some(min_length) = schema.get("minLength") {
        validate_string_length(value, min_length, path, "minLength", |actual, minimum| {
            actual >= minimum
        })?;
    }
    if let Some(max_length) = schema.get("maxLength") {
        validate_string_length(value, max_length, path, "maxLength", |actual, maximum| {
            actual <= maximum
        })?;
    }
    if let Some(min_items) = schema.get("minItems") {
        validate_array_length(value, min_items, path, "minItems", |actual, minimum| {
            actual >= minimum
        })?;
    }
    if let Some(max_items) = schema.get("maxItems") {
        validate_array_length(value, max_items, path, "maxItems", |actual, maximum| {
            actual <= maximum
        })?;
    }

    if let Value::Object(object) = value {
        validate_object_keywords(object, schema, path)?;
    }
    if let Value::Array(items) = value {
        validate_array_keywords(items, schema, path)?;
    }

    Ok(())
}

fn validate_type_keyword(
    value: &Value,
    type_keyword: &Value,
    path: &str,
) -> Result<(), JsonSchemaValidationError> {
    let valid = match type_keyword {
        Value::String(expected) => value_matches_schema_type(value, expected),
        Value::Array(options) => options.iter().any(|option| {
            option
                .as_str()
                .is_some_and(|expected| value_matches_schema_type(value, expected))
        }),
        _ => {
            return Err(JsonSchemaValidationError::new(format!(
                "schema at {path} has invalid `type`; expected string or array"
            )))
        }
    };

    if valid {
        return Ok(());
    }

    Err(JsonSchemaValidationError::new(format!(
        "{path} expected {} but found {}",
        summarize_type_keyword(type_keyword),
        describe_value_type(value)
    )))
}

fn validate_enum_keyword(
    value: &Value,
    enum_keyword: &Value,
    path: &str,
) -> Result<(), JsonSchemaValidationError> {
    let options = enum_keyword.as_array().ok_or_else(|| {
        JsonSchemaValidationError::new(format!(
            "schema at {path} has invalid `enum`; expected array"
        ))
    })?;
    if options.iter().any(|option| option == value) {
        return Ok(());
    }

    Err(JsonSchemaValidationError::new(format!(
        "{path} must be one of {}",
        options
            .iter()
            .map(summarize_json_value)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

fn validate_any_of(
    value: &Value,
    keyword: &Value,
    path: &str,
) -> Result<(), JsonSchemaValidationError> {
    let branches = keyword.as_array().ok_or_else(|| {
        JsonSchemaValidationError::new(format!(
            "schema at {path} has invalid `anyOf`; expected array"
        ))
    })?;
    let mut first_error = None;
    for branch in branches {
        let Some(branch_object) = branch.as_object() else {
            return Err(JsonSchemaValidationError::new(format!(
                "schema at {path} has invalid `anyOf`; each entry must be an object"
            )));
        };
        match validate_schema_node(value, branch_object, path) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }

    Err(first_error.unwrap_or_else(|| {
        JsonSchemaValidationError::new(format!("{path} did not match any allowed schema branch"))
    }))
}

fn validate_all_of(
    value: &Value,
    keyword: &Value,
    path: &str,
) -> Result<(), JsonSchemaValidationError> {
    let branches = keyword.as_array().ok_or_else(|| {
        JsonSchemaValidationError::new(format!(
            "schema at {path} has invalid `allOf`; expected array"
        ))
    })?;
    for branch in branches {
        let Some(branch_object) = branch.as_object() else {
            return Err(JsonSchemaValidationError::new(format!(
                "schema at {path} has invalid `allOf`; each entry must be an object"
            )));
        };
        validate_schema_node(value, branch_object, path)?;
    }
    Ok(())
}

fn validate_one_of(
    value: &Value,
    keyword: &Value,
    path: &str,
) -> Result<(), JsonSchemaValidationError> {
    let branches = keyword.as_array().ok_or_else(|| {
        JsonSchemaValidationError::new(format!(
            "schema at {path} has invalid `oneOf`; expected array"
        ))
    })?;
    let mut matches = 0;
    for branch in branches {
        let Some(branch_object) = branch.as_object() else {
            return Err(JsonSchemaValidationError::new(format!(
                "schema at {path} has invalid `oneOf`; each entry must be an object"
            )));
        };
        if validate_schema_node(value, branch_object, path).is_ok() {
            matches += 1;
        }
    }
    if matches == 1 {
        Ok(())
    } else {
        Err(JsonSchemaValidationError::new(format!(
            "{path} matched {matches} `oneOf` branches"
        )))
    }
}

fn validate_numeric_bound(
    value: &Value,
    bound_keyword: &Value,
    path: &str,
    keyword_name: &str,
    compare: impl Fn(f64, f64) -> bool,
) -> Result<(), JsonSchemaValidationError> {
    let Some(actual) = value.as_f64() else {
        return Ok(());
    };
    let bound = bound_keyword.as_f64().ok_or_else(|| {
        JsonSchemaValidationError::new(format!(
            "schema at {path} has invalid `{keyword_name}`; expected number"
        ))
    })?;
    if compare(actual, bound) {
        Ok(())
    } else {
        Err(JsonSchemaValidationError::new(format!(
            "{path} must satisfy `{keyword_name} = {bound}`"
        )))
    }
}

fn validate_string_length(
    value: &Value,
    bound_keyword: &Value,
    path: &str,
    keyword_name: &str,
    compare: impl Fn(usize, usize) -> bool,
) -> Result<(), JsonSchemaValidationError> {
    let Some(actual) = value.as_str() else {
        return Ok(());
    };
    let bound = schema_usize_keyword(bound_keyword, keyword_name, path)?;
    let actual = actual.chars().count();
    if compare(actual, bound) {
        Ok(())
    } else {
        Err(JsonSchemaValidationError::new(format!(
            "{path} must satisfy `{keyword_name} = {bound}`"
        )))
    }
}

fn validate_array_length(
    value: &Value,
    bound_keyword: &Value,
    path: &str,
    keyword_name: &str,
    compare: impl Fn(usize, usize) -> bool,
) -> Result<(), JsonSchemaValidationError> {
    let Some(items) = value.as_array() else {
        return Ok(());
    };
    let bound = schema_usize_keyword(bound_keyword, keyword_name, path)?;
    if compare(items.len(), bound) {
        Ok(())
    } else {
        Err(JsonSchemaValidationError::new(format!(
            "{path} must satisfy `{keyword_name} = {bound}`"
        )))
    }
}

fn validate_object_keywords(
    value: &Map<String, Value>,
    schema: &Map<String, Value>,
    path: &str,
) -> Result<(), JsonSchemaValidationError> {
    let properties = schema
        .get("properties")
        .map(|properties| {
            properties.as_object().ok_or_else(|| {
                JsonSchemaValidationError::new(format!(
                    "schema at {path} has invalid `properties`; expected object"
                ))
            })
        })
        .transpose()?;

    if let Some(required) = schema.get("required") {
        let required = required.as_array().ok_or_else(|| {
            JsonSchemaValidationError::new(format!(
                "schema at {path} has invalid `required`; expected array"
            ))
        })?;
        for key in required {
            let Some(key) = key.as_str() else {
                return Err(JsonSchemaValidationError::new(format!(
                    "schema at {path} has invalid `required`; entries must be strings"
                )));
            };
            if !value.contains_key(key) {
                return Err(JsonSchemaValidationError::new(format!(
                    "{path} is missing required property `{key}`"
                )));
            }
        }
    }

    if let Some(properties) = properties {
        for (key, property_schema) in properties {
            let Some(property_value) = value.get(key) else {
                continue;
            };
            let property_schema = property_schema.as_object().ok_or_else(|| {
                JsonSchemaValidationError::new(format!(
                    "schema at {path}.properties.{key} must be an object"
                ))
            })?;
            validate_schema_node(property_value, property_schema, &format!("{path}.{key}"))?;
        }
    }

    let additional_properties = schema.get("additionalProperties");
    for (key, property_value) in value {
        if properties.is_some_and(|properties| properties.contains_key(key)) {
            continue;
        }
        match additional_properties {
            Some(Value::Bool(false)) => {
                return Err(JsonSchemaValidationError::new(format!(
                    "{path} does not allow additional property `{key}`"
                )))
            }
            Some(Value::Object(property_schema)) => {
                validate_schema_node(property_value, property_schema, &format!("{path}.{key}"))?;
            }
            Some(Value::Bool(true)) | None => {}
            Some(_) => {
                return Err(JsonSchemaValidationError::new(format!(
                    "schema at {path} has invalid `additionalProperties`"
                )))
            }
        }
    }

    Ok(())
}

fn validate_array_keywords(
    value: &[Value],
    schema: &Map<String, Value>,
    path: &str,
) -> Result<(), JsonSchemaValidationError> {
    let Some(items) = schema.get("items") else {
        return Ok(());
    };
    match items {
        Value::Object(item_schema) => {
            for (index, item) in value.iter().enumerate() {
                validate_schema_node(item, item_schema, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Array(item_schemas) => {
            for (index, item) in value.iter().enumerate() {
                let Some(item_schema) = item_schemas.get(index) else {
                    break;
                };
                let item_schema = item_schema.as_object().ok_or_else(|| {
                    JsonSchemaValidationError::new(format!(
                        "schema at {path}.items[{index}] must be an object"
                    ))
                })?;
                validate_schema_node(item, item_schema, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        _ => Err(JsonSchemaValidationError::new(format!(
            "schema at {path} has invalid `items`; expected object or array"
        ))),
    }
}

fn value_matches_schema_type(value: &Value, expected: &str) -> bool {
    match expected {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_f64().is_some_and(|number| number.fract() == 0.0),
        _ => false,
    }
}

fn summarize_type_keyword(type_keyword: &Value) -> String {
    match type_keyword {
        Value::String(value) => format!("type `{value}`"),
        Value::Array(values) => {
            let values = values
                .iter()
                .filter_map(Value::as_str)
                .map(|value| format!("`{value}`"))
                .collect::<Vec<_>>();
            format!("one of {}", values.join(", "))
        }
        _ => "a valid schema type".to_string(),
    }
}

fn describe_value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) => {
            if number.as_f64().is_some_and(|value| value.fract() == 0.0) {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn summarize_json_value(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| describe_value_type(value).to_string())
}

fn schema_usize_keyword(
    value: &Value,
    keyword_name: &str,
    path: &str,
) -> Result<usize, JsonSchemaValidationError> {
    let Some(value) = value.as_u64() else {
        return Err(JsonSchemaValidationError::new(format!(
            "schema at {path} has invalid `{keyword_name}`; expected non-negative integer"
        )));
    };
    usize::try_from(value).map_err(|_| {
        JsonSchemaValidationError::new(format!(
            "schema at {path} has out-of-range `{keyword_name}`"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::validate_json_value_against_schema;
    use serde_json::json;

    #[test]
    fn validates_required_properties_and_additional_properties() {
        let schema = json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string" }
            },
            "additionalProperties": false
        });

        validate_json_value_against_schema(&json!({ "name": "alpha" }), &schema)
            .expect("schema should accept valid input");

        let error = validate_json_value_against_schema(&json!({}), &schema)
            .expect_err("missing required property should fail");
        assert!(error
            .to_string()
            .contains("missing required property `name`"));

        let error =
            validate_json_value_against_schema(&json!({ "name": "alpha", "extra": true }), &schema)
                .expect_err("additional property should fail");
        assert!(error
            .to_string()
            .contains("does not allow additional property `extra`"));
    }

    #[test]
    fn validates_arrays_with_item_schemas() {
        let schema = json!({
            "type": "array",
            "items": { "type": "integer" },
            "minItems": 1
        });

        validate_json_value_against_schema(&json!([1, 2, 3]), &schema)
            .expect("array should validate");

        let error = validate_json_value_against_schema(&json!([]), &schema)
            .expect_err("minItems should fail");
        assert!(error.to_string().contains("minItems"));

        let error = validate_json_value_against_schema(&json!([1, 2.5]), &schema)
            .expect_err("item schema should fail");
        assert!(error.to_string().contains("$[1]"));
    }

    #[test]
    fn validates_enums_and_consts() {
        let schema = json!({
            "enum": ["draft", "ready"],
            "const": "ready"
        });

        validate_json_value_against_schema(&json!("ready"), &schema)
            .expect("matching const should validate");

        let error = validate_json_value_against_schema(&json!("draft"), &schema)
            .expect_err("const mismatch should fail");
        assert!(error.to_string().contains("must equal \"ready\""));
    }

    #[test]
    fn validates_any_of_and_one_of() {
        let any_of_schema = json!({
            "anyOf": [
                { "type": "string" },
                { "type": "integer" }
            ]
        });
        validate_json_value_against_schema(&json!("alpha"), &any_of_schema)
            .expect("string branch should validate");
        validate_json_value_against_schema(&json!(7), &any_of_schema)
            .expect("integer branch should validate");
        validate_json_value_against_schema(&json!(true), &any_of_schema)
            .expect_err("boolean should not match any branch");

        let one_of_schema = json!({
            "oneOf": [
                { "type": "integer" },
                { "type": "number" }
            ]
        });
        let error = validate_json_value_against_schema(&json!(7), &one_of_schema)
            .expect_err("integer matches both branches");
        assert!(error.to_string().contains("matched 2 `oneOf` branches"));
    }

    #[test]
    fn validates_nested_object_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "object",
                    "required": ["title"],
                    "properties": {
                        "title": { "type": "string", "minLength": 1 },
                        "done": { "type": "boolean" }
                    },
                    "additionalProperties": false
                }
            }
        });

        validate_json_value_against_schema(
            &json!({ "task": { "title": "Ship", "done": false } }),
            &schema,
        )
        .expect("nested object should validate");

        let error = validate_json_value_against_schema(
            &json!({ "task": { "title": "", "done": false } }),
            &schema,
        )
        .expect_err("nested minLength should fail");
        assert!(error.to_string().contains("$.task.title"));
    }
}
