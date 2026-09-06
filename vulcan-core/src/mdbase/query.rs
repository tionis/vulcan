use super::{
    bundled_mdbase_schema, validate_mdbase_schema_value, MdbaseCelEngine, MdbaseDiagnostic,
    MdbaseDiagnosticLevel, MDBASE_CANONICAL_SCHEMA_BASE,
};
use crate::query::{
    QueryDirection, QueryExpressionLanguage, QueryExpressionSpec, QueryFrontmatterMode,
    QueryNamedExpression, QueryOrderKey, QuerySelection, QuerySource, QuerySummarySpec,
    StructuredQueryPlan,
};
use chrono_tz::Tz;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq)]
pub struct MdbaseQueryError {
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

impl Display for MdbaseQueryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            &self
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

impl std::error::Error for MdbaseQueryError {}

#[derive(Debug, Default, Deserialize)]
struct PortableQuery {
    #[serde(default)]
    types: Vec<String>,
    timezone: Option<String>,
    context: Option<PortableContext>,
    #[serde(default)]
    projections: BTreeMap<String, PortableExpression>,
    #[serde(rename = "where")]
    filter: Option<String>,
    select: Option<Vec<PortableSelection>>,
    #[serde(default)]
    order_by: Vec<PortableOrder>,
    #[serde(default)]
    group_by: Vec<PortableOrder>,
    #[serde(default)]
    summary_functions: BTreeMap<String, PortableExpression>,
    #[serde(default)]
    summaries: Vec<PortableSummary>,
    limit: Option<usize>,
    #[serde(default)]
    offset: usize,
    #[serde(default)]
    include_body: bool,
    #[serde(default)]
    frontmatter_mode: QueryFrontmatterMode,
}

#[derive(Debug, Deserialize)]
struct PortableContext {
    this: PortableThis,
}

#[derive(Debug, Deserialize)]
struct PortableThis {
    path: String,
}

#[derive(Debug, Deserialize)]
struct PortableExpression {
    expr: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PortableSelection {
    Field(String),
    Expression {
        name: String,
        expr: String,
        label: Option<String>,
        description: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct PortableOrder {
    field: String,
    #[serde(default)]
    direction: QueryDirection,
}

#[derive(Debug, Deserialize)]
struct PortableSummary {
    field: String,
    function: String,
    name: Option<String>,
    label: Option<String>,
}

/// Validate and compile a canonical mdbase query into Vulcan's rich internal
/// query plan. The mdbase object remains a frontend syntax, not a replacement
/// for Vulcan's compact `QueryAst` or human DSL.
pub fn compile_mdbase_query(
    value: &serde_json::Value,
) -> Result<StructuredQueryPlan, MdbaseQueryError> {
    validate_query_schema(value)?;
    let query = serde_json::from_value::<PortableQuery>(value.clone())
        .map_err(|error| query_error("invalid_query", error.to_string(), None, None))?;
    if let Some(timezone) = query.timezone.as_deref() {
        timezone.parse::<Tz>().map_err(|_| {
            query_error(
                "invalid_query",
                format!("`{timezone}` is not an IANA timezone identifier"),
                Some("timezone"),
                None,
            )
        })?;
    }

    let engine = MdbaseCelEngine::default();
    let named_projections = order_named_expressions(&engine, query.projections, "projections")?;
    let summary_functions =
        order_named_expressions(&engine, query.summary_functions, "summary_functions")?;
    let filter = query
        .filter
        .map(|source| compile_expression(&engine, source, "where"))
        .transpose()?;

    let selection = query
        .select
        .map(|selections| compile_selections(&engine, selections))
        .transpose()?;
    let summaries = compile_summaries(query.summaries)?;

    Ok(StructuredQueryPlan {
        source: QuerySource::Notes,
        types: query.types,
        timezone: query.timezone,
        invocation_context: query.context.map(|context| context.this.path),
        named_projections,
        filter,
        selection,
        order_by: map_order(query.order_by),
        group_by: map_order(query.group_by),
        summary_functions,
        summaries,
        frontmatter_mode: query.frontmatter_mode,
        include_body: query.include_body,
        limit: query.limit,
        offset: query.offset,
    })
}

fn compile_selections(
    engine: &MdbaseCelEngine,
    selections: Vec<PortableSelection>,
) -> Result<Vec<QuerySelection>, MdbaseQueryError> {
    let mut output_names = BTreeSet::new();
    selections
        .into_iter()
        .map(|selection| {
            let selection = match selection {
                PortableSelection::Field(field) => {
                    let output_name = field.rsplit('.').next().unwrap_or(&field).to_string();
                    QuerySelection::Field { field, output_name }
                }
                PortableSelection::Expression {
                    name,
                    expr,
                    label,
                    description,
                } => QuerySelection::Expression {
                    name,
                    expression: compile_expression(engine, expr, "select")?,
                    label,
                    description,
                },
            };
            let name = match &selection {
                QuerySelection::Field { output_name, .. } => output_name,
                QuerySelection::Expression { name, .. } => name,
            };
            if !output_names.insert(name.clone()) {
                return Err(query_error(
                    "invalid_query",
                    format!("duplicate selection output name `{name}`"),
                    Some("select"),
                    None,
                ));
            }
            Ok(selection)
        })
        .collect()
}

fn compile_summaries(
    summaries: Vec<PortableSummary>,
) -> Result<Vec<QuerySummarySpec>, MdbaseQueryError> {
    let mut summary_names = BTreeSet::new();
    summaries
        .into_iter()
        .map(|summary| {
            let output_name = summary.name.unwrap_or_else(|| summary.function.clone());
            if !summary_names.insert(output_name.clone()) {
                return Err(query_error(
                    "invalid_query",
                    format!("duplicate summary output name `{output_name}`"),
                    Some("summaries"),
                    None,
                ));
            }
            Ok(QuerySummarySpec {
                field: summary.field,
                function: summary.function,
                output_name,
                label: summary.label,
            })
        })
        .collect()
}

fn validate_query_schema(value: &serde_json::Value) -> Result<(), MdbaseQueryError> {
    let schema = bundled_mdbase_schema(&format!("{MDBASE_CANONICAL_SCHEMA_BASE}query.schema.json"))
        .expect("query schema is bundled");
    let schema = serde_json::from_str(schema.json).expect("bundled query schema is valid JSON");
    let diagnostics = validate_mdbase_schema_value(&schema, value)
        .expect("bundled query schema compiles")
        .into_iter()
        .map(|diagnostic| MdbaseDiagnostic {
            severity: MdbaseDiagnosticLevel::Error,
            code: "invalid_query".to_string(),
            message: diagnostic.message,
            path: None,
            field: Some(diagnostic.instance_path),
            type_name: None,
            schema_location: Some(diagnostic.schema_path),
            details: Some(serde_json::json!({"schema_code": diagnostic.code})),
        })
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(MdbaseQueryError { diagnostics })
    }
}

fn order_named_expressions(
    engine: &MdbaseCelEngine,
    expressions: BTreeMap<String, PortableExpression>,
    field: &str,
) -> Result<Vec<QueryNamedExpression>, MdbaseQueryError> {
    let mut pending = expressions
        .into_iter()
        .map(|(name, expression)| {
            let program = engine.compile(&expression.expr).map_err(|error| {
                query_error(
                    "invalid_query",
                    format!("{}: {}", error.code, error.message),
                    Some(field),
                    Some(&name),
                )
            })?;
            let dependencies = program
                .projection_dependencies()
                .map(str::to_string)
                .collect::<BTreeSet<_>>();
            Ok((name, (expression, dependencies)))
        })
        .collect::<Result<BTreeMap<_, _>, MdbaseQueryError>>()?;
    let names = pending.keys().cloned().collect::<BTreeSet<_>>();
    let mut resolved = BTreeSet::new();
    let mut ordered = Vec::new();
    while !pending.is_empty() {
        let next = pending
            .iter()
            .find(|(_, (_, dependencies))| {
                dependencies
                    .iter()
                    .filter(|dependency| names.contains(*dependency))
                    .all(|dependency| resolved.contains(dependency))
            })
            .map(|(name, _)| name.clone());
        let Some(name) = next else {
            return Err(query_error(
                "invalid_query",
                "named projection dependency cycle",
                Some(field),
                None,
            ));
        };
        let (expression, _) = pending.remove(&name).expect("selected pending expression");
        resolved.insert(name.clone());
        ordered.push(QueryNamedExpression {
            name,
            expression: cel_expression(expression.expr),
            description: expression.description,
        });
    }
    Ok(ordered)
}

fn compile_expression(
    engine: &MdbaseCelEngine,
    source: String,
    field: &str,
) -> Result<QueryExpressionSpec, MdbaseQueryError> {
    engine.compile(&source).map_err(|error| {
        query_error(
            "invalid_query",
            format!("{}: {}", error.code, error.message),
            Some(field),
            None,
        )
    })?;
    Ok(cel_expression(source))
}

fn cel_expression(source: String) -> QueryExpressionSpec {
    QueryExpressionSpec {
        language: QueryExpressionLanguage::Cel,
        source,
    }
}

fn map_order(values: Vec<PortableOrder>) -> Vec<QueryOrderKey> {
    values
        .into_iter()
        .map(|value| QueryOrderKey {
            field: value.field,
            direction: value.direction,
        })
        .collect()
}

fn query_error(
    code: &str,
    message: impl Into<String>,
    field: Option<&str>,
    name: Option<&str>,
) -> MdbaseQueryError {
    MdbaseQueryError {
        diagnostics: vec![MdbaseDiagnostic {
            severity: MdbaseDiagnosticLevel::Error,
            code: code.to_string(),
            message: message.into(),
            path: None,
            field: field.map(ToString::to_string),
            type_name: None,
            schema_location: None,
            details: name.map(|name| serde_json::json!({"name": name})),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_portable_query_compiles_to_the_shared_rich_plan() {
        let yaml: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../resources/mdbase/v0.3/upstream/tests/fixtures/views/valid-query.yml"
        ))
        .expect("fixture YAML");
        let value = serde_json::to_value(yaml).expect("JSON-compatible fixture");

        let plan = compile_mdbase_query(&value).expect("query compiles");

        assert_eq!(plan.types, ["task"]);
        assert_eq!(
            plan.invocation_context.as_deref(),
            Some("projects/alpha.md")
        );
        assert_eq!(plan.named_projections[0].name, "is_overdue");
        assert_eq!(plan.order_by.len(), 1);
        assert_eq!(plan.group_by.len(), 1);
        assert_eq!(plan.summaries[0].output_name, "completion_rate");
        assert_eq!(plan.frontmatter_mode, QueryFrontmatterMode::Both);
        assert_eq!(plan.limit, Some(20));
    }

    #[test]
    fn projection_dependencies_are_ordered_and_cycles_are_rejected() {
        let ordered = serde_json::json!({
            "projections": {
                "second": {"expr": "projection.first + 1"},
                "first": {"expr": "1"}
            }
        });
        let plan = compile_mdbase_query(&ordered).expect("dependencies resolve");
        assert_eq!(
            plan.named_projections
                .iter()
                .map(|projection| projection.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );

        let cycle = serde_json::json!({
            "projections": {
                "first": {"expr": "projection.second"},
                "second": {"expr": "projection.first"}
            }
        });
        let error = compile_mdbase_query(&cycle).expect_err("cycle is invalid");
        assert_eq!(error.diagnostics[0].code, "invalid_query");
    }

    #[test]
    fn duplicate_outputs_invalid_timezone_and_expression_fail_preflight() {
        for query in [
            serde_json::json!({"select": ["file.path", "path"]}),
            serde_json::json!({"timezone": "+02:00"}),
            serde_json::json!({"where": "status =="}),
            serde_json::json!({
                "summaries": [
                    {"field": "a", "function": "count"},
                    {"field": "b", "function": "count"}
                ]
            }),
        ] {
            let error = compile_mdbase_query(&query).expect_err("query should be invalid");
            assert_eq!(error.diagnostics[0].code, "invalid_query");
        }
    }
}
