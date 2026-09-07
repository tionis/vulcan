use super::{
    bundled_mdbase_schema, validate_mdbase_schema_value, MdbaseCelClock, MdbaseCelContext,
    MdbaseCelContextKind, MdbaseCelEngine, MdbaseCelLinkIndex, MdbaseDiagnostic,
    MdbaseDiagnosticLevel, MdbaseRecordDocument, MdbaseRecordSet, MdbaseTypeRegistry,
    MDBASE_CANONICAL_SCHEMA_BASE,
};
use crate::query::{
    QueryDirection, QueryExpressionLanguage, QueryExpressionSpec, QueryFrontmatterMode,
    QueryNamedExpression, QueryOrderKey, QuerySelection, QuerySource, QuerySummarySpec,
    StructuredQueryPlan,
};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq)]
pub struct MdbaseQueryError {
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseQueryResult {
    pub results: Vec<MdbaseQueryRow>,
    pub meta: MdbaseQueryMeta,
    pub diagnostics: Vec<MdbaseDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseQueryRow {
    pub file: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_frontmatter: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseQueryMeta {
    pub total_count: usize,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<MdbaseQueryContextMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<MdbaseQueryGroup>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MdbaseQueryContextMeta {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MdbaseQueryGroup {
    pub values: serde_json::Map<String, serde_json::Value>,
    pub count: usize,
    pub summaries: serde_json::Map<String, serde_json::Value>,
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

pub fn execute_mdbase_query(
    records: &MdbaseRecordSet,
    types: &MdbaseTypeRegistry,
    plan: &StructuredQueryPlan,
    id_field: &str,
    collection_timezone: Option<&str>,
    now: DateTime<Utc>,
) -> Result<MdbaseQueryResult, MdbaseQueryError> {
    let timezone = plan
        .timezone
        .as_deref()
        .or(collection_timezone)
        .unwrap_or("UTC");
    let clock = MdbaseCelClock::new(now, timezone)
        .map_err(|error| query_error("invalid_query", error.message, Some("timezone"), None))?;
    let invocation_context = plan
        .invocation_context
        .as_deref()
        .map(|path| {
            records.get(path).ok_or_else(|| {
                query_error(
                    "context_not_found",
                    format!("query context record was not found: {path}"),
                    Some("context.this.path"),
                    None,
                )
            })
        })
        .transpose()?;
    let engine = MdbaseCelEngine::default();
    let link_index = Arc::new(MdbaseCelLinkIndex::new(records, id_field));
    let mut diagnostics = Vec::new();
    let mut candidates = Vec::new();
    for record in &records.records {
        if let Some(candidate) = evaluate_query_candidate(
            &engine,
            plan,
            record,
            types,
            invocation_context,
            &clock,
            &link_index,
            &mut diagnostics,
        )? {
            candidates.push(candidate);
        }
    }
    candidates.sort_by(|left, right| compare_candidates(left, right, &plan.order_by));
    let total_count = candidates.len();
    let groups = build_groups(&engine, &candidates, plan, &clock, &mut diagnostics)?;
    let start = plan.offset.min(total_count);
    let end = plan.limit.map_or(total_count, |limit| {
        start.saturating_add(limit).min(total_count)
    });
    let results = candidates[start..end]
        .iter()
        .map(|candidate| query_row(candidate, plan))
        .collect();
    Ok(MdbaseQueryResult {
        results,
        meta: MdbaseQueryMeta {
            total_count,
            has_more: end < total_count,
            context: invocation_context.map(|record| MdbaseQueryContextMeta {
                path: record.path.clone(),
            }),
            groups,
        },
        diagnostics,
    })
}

struct QueryCandidate<'a> {
    record: &'a MdbaseRecordDocument,
    projection: serde_json::Map<String, serde_json::Value>,
    values: Option<serde_json::Map<String, serde_json::Value>>,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_query_candidate<'a>(
    engine: &MdbaseCelEngine,
    plan: &StructuredQueryPlan,
    record: &'a MdbaseRecordDocument,
    types: &MdbaseTypeRegistry,
    invocation_context: Option<&MdbaseRecordDocument>,
    clock: &MdbaseCelClock,
    link_index: &Arc<MdbaseCelLinkIndex>,
    diagnostics: &mut Vec<MdbaseDiagnostic>,
) -> Result<Option<QueryCandidate<'a>>, MdbaseQueryError> {
    if !plan.types.is_empty()
        && !record.types.iter().any(|candidate| {
            plan.types
                .iter()
                .any(|wanted| candidate.eq_ignore_ascii_case(wanted))
        })
    {
        return Ok(None);
    }
    let known_fields = known_fields(record, types);
    let mut projection = plan
        .named_projections
        .iter()
        .map(|projection| (projection.name.clone(), serde_json::Value::Null))
        .collect::<serde_json::Map<_, _>>();
    for named in &plan.named_projections {
        let context = MdbaseCelContext::query(
            MdbaseCelContextKind::QueryProjection,
            record,
            known_fields.iter().cloned(),
            serde_json::Value::Object(projection.clone()),
            invocation_context,
            clock.clone(),
        )
        .map_err(cel_query_error)?;
        let context = context.with_link_index(Arc::clone(link_index));
        let result = engine
            .evaluate_context(
                &engine
                    .compile(&named.expression.source)
                    .map_err(cel_query_error)?,
                &context,
            )
            .map_err(cel_query_error)?;
        projection.insert(named.name.clone(), result.value);
        diagnostics.extend(result.diagnostics);
    }
    if let Some(filter) = &plan.filter {
        let context = MdbaseCelContext::query(
            MdbaseCelContextKind::QueryFilter,
            record,
            known_fields.iter().cloned(),
            serde_json::Value::Object(projection.clone()),
            invocation_context,
            clock.clone(),
        )
        .map_err(cel_query_error)?;
        let context = context.with_link_index(Arc::clone(link_index));
        let result = engine
            .evaluate_context(
                &engine.compile(&filter.source).map_err(cel_query_error)?,
                &context,
            )
            .map_err(cel_query_error)?;
        diagnostics.extend(result.diagnostics);
        if !result.value.is_boolean() && !result.value.is_null() {
            diagnostics.push(query_diagnostic(
                "expression_evaluation_error",
                "query filter expression must return a boolean",
                Some("where"),
                None,
            ));
        }
        if result.value != serde_json::Value::Bool(true) {
            return Ok(None);
        }
    }
    let values = evaluate_selection(
        engine,
        plan,
        record,
        &known_fields,
        &projection,
        invocation_context,
        clock,
        link_index,
        diagnostics,
    )?;
    Ok(Some(QueryCandidate {
        record,
        projection,
        values,
    }))
}

fn known_fields(record: &MdbaseRecordDocument, types: &MdbaseTypeRegistry) -> BTreeSet<String> {
    record
        .types
        .iter()
        .filter_map(|name| types.get(name))
        .filter_map(|definition| definition.schema.get("properties")?.as_object())
        .flat_map(serde_json::Map::keys)
        .cloned()
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn evaluate_selection(
    engine: &MdbaseCelEngine,
    plan: &StructuredQueryPlan,
    record: &MdbaseRecordDocument,
    known_fields: &BTreeSet<String>,
    projection: &serde_json::Map<String, serde_json::Value>,
    invocation_context: Option<&MdbaseRecordDocument>,
    clock: &MdbaseCelClock,
    link_index: &Arc<MdbaseCelLinkIndex>,
    diagnostics: &mut Vec<MdbaseDiagnostic>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, MdbaseQueryError> {
    let Some(selection) = &plan.selection else {
        return Ok(None);
    };
    let mut values = serde_json::Map::new();
    for selection in selection {
        match selection {
            QuerySelection::Field { field, output_name } => {
                values.insert(
                    output_name.clone(),
                    candidate_value(record, projection, None, field),
                );
            }
            QuerySelection::Expression {
                name, expression, ..
            } => {
                let context = MdbaseCelContext::query(
                    MdbaseCelContextKind::QueryProjection,
                    record,
                    known_fields.iter().cloned(),
                    serde_json::Value::Object(projection.clone()),
                    invocation_context,
                    clock.clone(),
                )
                .map_err(cel_query_error)?;
                let context = context.with_link_index(Arc::clone(link_index));
                let result = engine
                    .evaluate_context(
                        &engine
                            .compile(&expression.source)
                            .map_err(cel_query_error)?,
                        &context,
                    )
                    .map_err(cel_query_error)?;
                values.insert(name.clone(), result.value);
                diagnostics.extend(result.diagnostics);
            }
        }
    }
    Ok(Some(values))
}

fn candidate_value(
    record: &MdbaseRecordDocument,
    projection: &serde_json::Map<String, serde_json::Value>,
    values: Option<&serde_json::Map<String, serde_json::Value>>,
    field: &str,
) -> serde_json::Value {
    if let Some(field) = field.strip_prefix("projection.") {
        return projection.get(field).cloned().unwrap_or_default();
    }
    if let Some(field) = field.strip_prefix("file.") {
        return serde_json::to_value(&record.file)
            .ok()
            .and_then(|file| file.get(field).cloned())
            .unwrap_or_default();
    }
    if let Some(value) = values.and_then(|values| values.get(field)) {
        return value.clone();
    }
    record
        .effective_frontmatter
        .get(field)
        .cloned()
        .unwrap_or_default()
}

fn compare_candidates(
    left: &QueryCandidate<'_>,
    right: &QueryCandidate<'_>,
    ordering: &[QueryOrderKey],
) -> Ordering {
    for key in ordering {
        let left_value = candidate_value(
            left.record,
            &left.projection,
            left.values.as_ref(),
            &key.field,
        );
        let right_value = candidate_value(
            right.record,
            &right.projection,
            right.values.as_ref(),
            &key.field,
        );
        let order = compare_json(&left_value, &right_value, key.direction);
        if order != Ordering::Equal {
            return order;
        }
    }
    left.record.path.cmp(&right.record.path)
}

fn compare_json(
    left: &serde_json::Value,
    right: &serde_json::Value,
    direction: QueryDirection,
) -> Ordering {
    let order = match (left, right) {
        (serde_json::Value::Null, serde_json::Value::Null) => Ordering::Equal,
        (serde_json::Value::Null, _) => Ordering::Greater,
        (_, serde_json::Value::Null) => Ordering::Less,
        (serde_json::Value::Number(left), serde_json::Value::Number(right)) => left
            .as_f64()
            .partial_cmp(&right.as_f64())
            .unwrap_or(Ordering::Equal),
        (serde_json::Value::String(left), serde_json::Value::String(right)) => left.cmp(right),
        (serde_json::Value::Bool(left), serde_json::Value::Bool(right)) => left.cmp(right),
        _ => left.to_string().cmp(&right.to_string()),
    };
    if direction == QueryDirection::Desc {
        order.reverse()
    } else {
        order
    }
}

fn build_groups(
    engine: &MdbaseCelEngine,
    candidates: &[QueryCandidate<'_>],
    plan: &StructuredQueryPlan,
    clock: &MdbaseCelClock,
    diagnostics: &mut Vec<MdbaseDiagnostic>,
) -> Result<Option<Vec<MdbaseQueryGroup>>, MdbaseQueryError> {
    if plan.group_by.is_empty() && plan.summaries.is_empty() {
        return Ok(None);
    }
    let mut buckets: Vec<(
        serde_json::Map<String, serde_json::Value>,
        Vec<&QueryCandidate<'_>>,
    )> = Vec::new();
    for candidate in candidates {
        let keys = plan
            .group_by
            .iter()
            .map(|group| {
                (
                    group.field.clone(),
                    candidate_value(
                        candidate.record,
                        &candidate.projection,
                        candidate.values.as_ref(),
                        &group.field,
                    ),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        if let Some((_, values)) = buckets.iter_mut().find(|(existing, _)| existing == &keys) {
            values.push(candidate);
        } else {
            buckets.push((keys, vec![candidate]));
        }
    }
    let mut groups = Vec::new();
    for (values, members) in buckets {
        let summaries = evaluate_summaries(engine, &members, plan, clock, diagnostics)?;
        groups.push(MdbaseQueryGroup {
            values,
            count: members.len(),
            summaries,
        });
    }
    groups.sort_by(|left, right| compare_group_values(left, right, &plan.group_by));
    Ok(Some(groups))
}

fn compare_group_values(
    left: &MdbaseQueryGroup,
    right: &MdbaseQueryGroup,
    ordering: &[QueryOrderKey],
) -> Ordering {
    for key in ordering {
        let order = compare_json(
            left.values
                .get(&key.field)
                .unwrap_or(&serde_json::Value::Null),
            right
                .values
                .get(&key.field)
                .unwrap_or(&serde_json::Value::Null),
            key.direction,
        );
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

fn evaluate_summaries(
    engine: &MdbaseCelEngine,
    members: &[&QueryCandidate<'_>],
    plan: &StructuredQueryPlan,
    clock: &MdbaseCelClock,
    diagnostics: &mut Vec<MdbaseDiagnostic>,
) -> Result<serde_json::Map<String, serde_json::Value>, MdbaseQueryError> {
    let custom = plan
        .summary_functions
        .iter()
        .map(|function| (function.name.as_str(), &function.expression.source))
        .collect::<BTreeMap<_, _>>();
    let mut output = serde_json::Map::new();
    for summary in &plan.summaries {
        let values = members
            .iter()
            .map(|candidate| {
                candidate_value(
                    candidate.record,
                    &candidate.projection,
                    candidate.values.as_ref(),
                    &summary.field,
                )
            })
            .collect::<Vec<_>>();
        let value = if let Some(source) = custom.get(summary.function.as_str()) {
            let context = MdbaseCelContext::system(
                MdbaseCelContextKind::QuerySummary,
                BTreeMap::from([("values".to_string(), serde_json::Value::Array(values))]),
                clock.clone(),
            )
            .map_err(cel_query_error)?;
            let result = engine
                .evaluate_context(&engine.compile(source).map_err(cel_query_error)?, &context)
                .map_err(cel_query_error)?;
            diagnostics.extend(result.diagnostics);
            result.value
        } else {
            match builtin_summary(&summary.function, &values) {
                Ok(value) => value,
                Err(message) => {
                    diagnostics.push(query_diagnostic(
                        "expression_evaluation_error",
                        message,
                        Some("summaries"),
                        Some(&summary.output_name),
                    ));
                    serde_json::Value::Null
                }
            }
        };
        output.insert(summary.output_name.clone(), value);
    }
    Ok(output)
}

fn builtin_summary(
    function: &str,
    values: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let non_null = values
        .iter()
        .filter(|value| !value.is_null())
        .collect::<Vec<_>>();
    match function {
        "count" => Ok(serde_json::json!(values.len())),
        "empty" => Ok(serde_json::json!(values
            .iter()
            .filter(|value| value_is_empty(value))
            .count())),
        "filled" => Ok(serde_json::json!(values
            .iter()
            .filter(|value| !value_is_empty(value))
            .count())),
        "sum" | "average" => {
            if non_null.is_empty() {
                return Ok(serde_json::Value::Null);
            }
            let numbers = non_null
                .iter()
                .map(|value| {
                    value
                        .as_f64()
                        .ok_or_else(|| format!("{function} requires numeric values"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let sum = numbers.iter().sum::<f64>();
            let count = u32::try_from(numbers.len())
                .map(f64::from)
                .map_err(|_| "summary contains too many values".to_string())?;
            Ok(serde_json::json!(if function == "average" {
                sum / count
            } else {
                sum
            }))
        }
        "minimum" | "earliest" | "maximum" | "latest" => {
            let mut values = non_null.into_iter();
            let Some(mut result) = values.next() else {
                return Ok(serde_json::Value::Null);
            };
            for value in values {
                let order = compare_json(value, result, QueryDirection::Asc);
                if (matches!(function, "minimum" | "earliest") && order == Ordering::Less)
                    || (matches!(function, "maximum" | "latest") && order == Ordering::Greater)
                {
                    result = value;
                }
            }
            Ok(result.clone())
        }
        _ => Err(format!("unknown summary function `{function}`")),
    }
}

fn value_is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(value) => value.is_empty(),
        serde_json::Value::Array(value) => value.is_empty(),
        serde_json::Value::Object(value) => value.is_empty(),
        _ => false,
    }
}

fn query_row(candidate: &QueryCandidate<'_>, plan: &StructuredQueryPlan) -> MdbaseQueryRow {
    MdbaseQueryRow {
        file: serde_json::json!({"path": candidate.record.path}),
        frontmatter: matches!(
            plan.frontmatter_mode,
            QueryFrontmatterMode::Persisted | QueryFrontmatterMode::Both
        )
        .then(|| candidate.record.frontmatter.clone()),
        effective_frontmatter: matches!(
            plan.frontmatter_mode,
            QueryFrontmatterMode::Effective | QueryFrontmatterMode::Both
        )
        .then(|| candidate.record.effective_frontmatter.clone()),
        values: candidate.values.clone().map(serde_json::Value::Object),
        body: plan.include_body.then(|| candidate.record.body.clone()),
    }
}

fn cel_query_error(error: super::MdbaseCelError) -> MdbaseQueryError {
    let super::MdbaseCelError { code, message } = error;
    query_error("invalid_query", format!("{code}: {message}"), None, None)
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
        diagnostics: vec![query_diagnostic(code, message, field, name)],
    }
}

fn query_diagnostic(
    code: &str,
    message: impl Into<String>,
    field: Option<&str>,
    name: Option<&str>,
) -> MdbaseDiagnostic {
    MdbaseDiagnostic {
        severity: MdbaseDiagnosticLevel::Error,
        code: code.to_string(),
        message: message.into(),
        path: None,
        field: field.map(ToString::to_string),
        type_name: None,
        schema_location: None,
        details: name.map(|name| serde_json::json!({"name": name})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::MdbaseRecordFileMetadata;

    fn record(path: &str, title: &str, status: &str) -> MdbaseRecordDocument {
        let frontmatter = serde_json::json!({
            "type": "task",
            "title": title,
            "status": status,
        });
        MdbaseRecordDocument {
            path: path.to_string(),
            revision: "revision".to_string(),
            types: vec!["task".to_string()],
            frontmatter: frontmatter.clone(),
            effective_frontmatter: frontmatter,
            body: format!("{title} body"),
            document: None,
            file: MdbaseRecordFileMetadata {
                path: path.to_string(),
                name: path.to_string(),
                basename: path.trim_end_matches(".md").to_string(),
                ext: "md".to_string(),
                folder: String::new(),
                size: 0,
                mtime: None,
                ctime: None,
            },
            links: Vec::new(),
            tags: Vec::new(),
            display: None,
            contract_views: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

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

    #[test]
    fn executor_filters_orders_pages_groups_and_keeps_summary_errors_non_fatal() {
        let records = MdbaseRecordSet {
            records: vec![
                record("b.md", "Beta", "open"),
                record("a.md", "Alpha", "open"),
                record("done.md", "Done", "done"),
            ],
        };
        let plan = compile_mdbase_query(&serde_json::json!({
            "where": "status == 'open'",
            "select": ["title"],
            "order_by": [{"field": "title"}],
            "group_by": [{"field": "status"}],
            "summaries": [{"field": "title", "function": "sum", "name": "bad_sum"}],
            "limit": 1,
            "offset": 0
        }))
        .expect("query compiles");
        let now = DateTime::parse_from_rfc3339("2026-06-14T08:15:00Z")
            .expect("timestamp")
            .with_timezone(&Utc);

        let result = execute_mdbase_query(
            &records,
            &MdbaseTypeRegistry::default(),
            &plan,
            "id",
            None,
            now,
        )
        .expect("query executes");

        assert_eq!(result.meta.total_count, 2);
        assert!(result.meta.has_more);
        assert_eq!(result.results[0].file["path"], "a.md");
        assert_eq!(result.meta.groups.as_ref().unwrap()[0].count, 2);
        assert_eq!(
            result.meta.groups.as_ref().unwrap()[0].summaries["bad_sum"],
            serde_json::Value::Null
        );
        assert_eq!(result.diagnostics[0].code, "expression_evaluation_error");
    }
}
