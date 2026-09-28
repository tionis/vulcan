//! Shared definitions for Container Core v1 exchange formats.
//!
//! MDAF v1 and Markdown Wiki Package v2 both use these source, locator,
//! selector, provenance, digest, and diagnostic rules. Format modules own
//! their manifests and member roles; this module owns only the semantics
//! that must not drift between formats.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExchangeDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExchangeDiagnostic {
    pub severity: ExchangeDiagnosticSeverity,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeProducer {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeSource {
    pub id: String,
    pub media_type: String,
    pub digest: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_digests: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeByteSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ExchangeSelector {
    Interval {
        unit: String,
        #[serde(serialize_with = "serialize_number")]
        start: f64,
        #[serde(serialize_with = "serialize_number")]
        end: f64,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_optional_number"
        )]
        origin: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label_start: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label_end: Option<String>,
    },
    Rectangle {
        unit: String,
        #[serde(serialize_with = "serialize_number")]
        x: f64,
        #[serde(serialize_with = "serialize_number")]
        y: f64,
        #[serde(serialize_with = "serialize_number")]
        width: f64,
        #[serde(serialize_with = "serialize_number")]
        height: f64,
    },
    Polygon {
        unit: String,
        points: Vec<ExchangePoint>,
    },
    Grid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sheet: Option<String>,
        row_start: u64,
        row_end: u64,
        column_start: u64,
        column_end: u64,
    },
    TextQuote {
        exact: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<String>,
    },
    Fragment {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        conforms_to: Option<String>,
    },
    Extension {
        namespace: String,
        data: serde_json::Value,
    },
}

impl ExchangeSelector {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Interval { .. } => "interval",
            Self::Rectangle { .. } => "rectangle",
            Self::Polygon { .. } => "polygon",
            Self::Grid { .. } => "grid",
            Self::TextQuote { .. } => "text-quote",
            Self::Fragment { .. } => "fragment",
            Self::Extension { .. } => "extension",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangePoint {
    #[serde(serialize_with = "serialize_number")]
    pub x: f64,
    #[serde(serialize_with = "serialize_number")]
    pub y: f64,
}

/// Largest magnitude at which every integer is exactly representable as f64.
const MAX_EXACT_INTEGER: f64 = 9_007_199_254_740_992.0;

/// Write integral coordinates as JSON/YAML integers (`74`, not `74.0`) so
/// locators copied into frontmatter read like the producer wrote them.
#[allow(clippy::cast_possible_truncation, clippy::trivially_copy_pass_by_ref)]
fn serialize_number<S: serde::Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    if value.fract() == 0.0 && value.abs() <= MAX_EXACT_INTEGER {
        serializer.serialize_i64(*value as i64)
    } else {
        serializer.serialize_f64(*value)
    }
}

#[allow(clippy::ref_option)]
fn serialize_optional_number<S: serde::Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => serialize_number(value, serializer),
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeSourceLocator {
    pub source_id: String,
    pub selectors: Vec<ExchangeSelector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeTool {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExchangeModelResolution {
    Pinned,
    MutableAlias,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeModel {
    pub provider: String,
    pub identifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returned_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
    pub resolution: ExchangeModelResolution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeActivity {
    pub id: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub tools: Vec<ExchangeTool>,
    pub models: Vec<ExchangeModel>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub depends_on: Vec<String>,
    pub parameters: serde_json::Map<String, Value>,
    pub parameters_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeRedaction {
    pub member: String,
    pub location: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeProvenance {
    pub version: u32,
    pub activities: Vec<ExchangeActivity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<ExchangeRedaction>,
}

/// One declared member as seen by provenance validation.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProvenanceMember<'a> {
    pub path: &'a str,
    pub created_by: &'a str,
}

/// Compile a bundled JSON Schema once so line-oriented members can validate
/// many records without recompiling it.
pub(crate) fn compile_schema(schema_text: &str) -> jsonschema::Validator {
    let schema: Value = serde_json::from_str(schema_text).expect("bundled schema is valid JSON");
    jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)
        .expect("bundled schema compiles")
}

pub(crate) fn validate_schema(
    member: &str,
    schema_text: &str,
    value: &Value,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) {
    validate_with(
        &compile_schema(schema_text),
        member,
        None,
        value,
        diagnostics,
    );
}

pub(crate) fn validate_with(
    validator: &jsonschema::Validator,
    member: &str,
    line: Option<usize>,
    value: &Value,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) {
    for error in validator.iter_errors(value) {
        let location = match line {
            Some(line) => format!("line {line}: {}", error.instance_path()),
            None => error.instance_path().to_string(),
        };
        error_diag(
            diagnostics,
            "schema_violation",
            format!("{location}: {error}"),
            member,
        );
    }
}

pub(crate) fn validate_locator(
    locator: &ExchangeSourceLocator,
    source_ids: &BTreeSet<&str>,
    path: &str,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) {
    if !source_ids.contains(locator.source_id.as_str()) {
        error_diag(
            diagnostics,
            "locator_source_unknown",
            format!("unknown source {}", locator.source_id),
            path,
        );
    }
    for selector in &locator.selectors {
        if !valid_selector(selector) {
            error_diag(
                diagnostics,
                "source_selector_invalid",
                "source selector is empty, non-finite, degenerate, out of bounds, or not namespaced",
                path,
            );
        }
    }
}

fn valid_selector(selector: &ExchangeSelector) -> bool {
    match selector {
        ExchangeSelector::Interval {
            unit,
            start,
            end,
            origin,
            ..
        } => {
            !unit.is_empty()
                && start.is_finite()
                && end.is_finite()
                && *start >= 0.0
                && start < end
                && origin.is_none_or(f64::is_finite)
        }
        ExchangeSelector::Rectangle {
            unit,
            x,
            y,
            width,
            height,
        } => {
            let finite = [x, y, width, height]
                .into_iter()
                .all(|value| value.is_finite());
            let bounded = match unit.as_str() {
                "percent" => *x + *width <= 100.0 && *y + *height <= 100.0,
                "normalized" => *x + *width <= 1.0 && *y + *height <= 1.0,
                _ => true,
            };
            !unit.is_empty()
                && finite
                && *x >= 0.0
                && *y >= 0.0
                && *width > 0.0
                && *height > 0.0
                && bounded
        }
        ExchangeSelector::Polygon { unit, points } => {
            !unit.is_empty()
                && points.len() >= 3
                && points
                    .iter()
                    .all(|point| point.x.is_finite() && point.y.is_finite())
                && polygon_area(points).abs() > f64::EPSILON
                && match unit.as_str() {
                    "percent" => points.iter().all(|point| {
                        (0.0..=100.0).contains(&point.x) && (0.0..=100.0).contains(&point.y)
                    }),
                    "normalized" => points.iter().all(|point| {
                        (0.0..=1.0).contains(&point.x) && (0.0..=1.0).contains(&point.y)
                    }),
                    _ => true,
                }
        }
        ExchangeSelector::Grid {
            row_start,
            row_end,
            column_start,
            column_end,
            ..
        } => row_start < row_end && column_start < column_end,
        ExchangeSelector::TextQuote { exact, .. } => !exact.is_empty(),
        ExchangeSelector::Fragment { value, conforms_to } => {
            !value.is_empty() && conforms_to.as_deref().is_none_or(|value| !value.is_empty())
        }
        ExchangeSelector::Extension { namespace, .. } => valid_namespace(namespace),
    }
}

fn polygon_area(points: &[ExchangePoint]) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(left, right)| left.x * right.y - right.x * left.y)
        .sum::<f64>()
        / 2.0
}

/// Namespaced identifiers use `reverse.domain/name`.
#[must_use]
pub fn valid_namespace(namespace: &str) -> bool {
    namespace
        .split_once('/')
        .is_some_and(|(authority, name)| authority.contains('.') && !name.is_empty())
}

/// Member namespaces are bare reverse-domain authorities such as
/// `dev.tionis.renwiki`.
#[must_use]
pub fn valid_member_namespace(namespace: &str) -> bool {
    let parts = namespace.split('.').collect::<Vec<_>>();
    parts.len() >= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

pub(crate) fn validate_span(
    span: ExchangeByteSpan,
    text: &str,
    path: &str,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) -> bool {
    let valid = span.start < span.end
        && span.end <= text.len()
        && text.is_char_boundary(span.start)
        && text.is_char_boundary(span.end);
    if !valid {
        error_diag(
            diagnostics,
            "document_span_invalid",
            format!("invalid UTF-8 byte span {}..{}", span.start, span.end),
            path,
        );
    }
    valid
}

pub(crate) fn validate_sources(
    sources: &[ExchangeSource],
    member_digests: &BTreeMap<&str, (&str, bool)>,
    manifest_path: &str,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) {
    let mut ids = BTreeSet::new();
    for source in sources {
        if !ids.insert(source.id.as_str()) {
            error_diag(
                diagnostics,
                "source_id_duplicate",
                "source ids must be unique",
                manifest_path,
            );
        }
        if let Some(path) = source.embedded_path.as_deref() {
            match member_digests.get(path) {
                Some((digest, true)) if *digest == source.digest => {}
                _ => error_diag(
                    diagnostics,
                    "embedded_source_invalid",
                    "embedded source must be declared with matching digest",
                    path,
                ),
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn validate_provenance(
    provenance: &ExchangeProvenance,
    members: &[ProvenanceMember<'_>],
    source_ids: &BTreeSet<&str>,
    provenance_path: &str,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) {
    let member_paths = members
        .iter()
        .map(|member| member.path)
        .collect::<BTreeSet<_>>();
    let activity_ids = provenance
        .activities
        .iter()
        .map(|activity| activity.id.as_str())
        .collect::<BTreeSet<_>>();
    if activity_ids.len() != provenance.activities.len() {
        error_diag(
            diagnostics,
            "activity_id_duplicate",
            "activity ids must be unique",
            provenance_path,
        );
    }
    let mut outputs_by_activity = BTreeMap::new();
    for activity in &provenance.activities {
        let expected = blake3_digest(&canonical_json(&Value::Object(activity.parameters.clone())));
        if expected != activity.parameters_digest {
            error_diag(
                diagnostics,
                "parameters_digest_mismatch",
                format!("parameter digest mismatch for {}", activity.id),
                provenance_path,
            );
        }
        for dependency in &activity.depends_on {
            if !activity_ids.contains(dependency.as_str()) || dependency == &activity.id {
                error_diag(
                    diagnostics,
                    "activity_dependency_invalid",
                    format!("invalid dependency {dependency}"),
                    provenance_path,
                );
            }
        }
        for output in &activity.outputs {
            if !member_paths.contains(output.as_str()) {
                error_diag(
                    diagnostics,
                    "activity_output_unknown",
                    format!("unknown output {output}"),
                    provenance_path,
                );
            }
        }
        for input in &activity.inputs {
            let source = input.strip_prefix("source:").unwrap_or(input);
            if !member_paths.contains(input.as_str()) && !source_ids.contains(source) {
                error_diag(
                    diagnostics,
                    "activity_input_unknown",
                    format!("unknown input {input}"),
                    provenance_path,
                );
            }
        }
        for model in &activity.models {
            if model.resolution != ExchangeModelResolution::Pinned {
                warning_diag(
                    diagnostics,
                    "model_not_pinned",
                    format!("model {} is not pinned", model.identifier),
                    provenance_path,
                );
            }
        }
        outputs_by_activity.insert(
            activity.id.as_str(),
            activity
                .outputs
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
        );
    }
    for member in members {
        if !outputs_by_activity
            .get(member.created_by)
            .is_some_and(|outputs| outputs.contains(member.path))
        {
            error_diag(
                diagnostics,
                "member_provenance_invalid",
                format!("{} is not emitted by {}", member.path, member.created_by),
                provenance_path,
            );
        }
    }
    if has_activity_cycle(&provenance.activities) {
        error_diag(
            diagnostics,
            "activity_cycle",
            "activity dependencies contain a cycle",
            provenance_path,
        );
    }
}

fn has_activity_cycle(activities: &[ExchangeActivity]) -> bool {
    fn visit<'a>(
        id: &'a str,
        graph: &BTreeMap<&'a str, &'a [String]>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
    ) -> bool {
        if visited.contains(id) {
            return false;
        }
        if !visiting.insert(id) {
            return true;
        }
        if graph.get(id).is_some_and(|dependencies| {
            dependencies
                .iter()
                .any(|dependency| visit(dependency, graph, visiting, visited))
        }) {
            return true;
        }
        visiting.remove(id);
        visited.insert(id);
        false
    }
    let graph = activities
        .iter()
        .map(|activity| (activity.id.as_str(), activity.depends_on.as_slice()))
        .collect::<BTreeMap<_, _>>();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    graph
        .keys()
        .any(|id| visit(id, &graph, &mut visiting, &mut visited))
}

/// Container Core logical identity: BLAKE3 over one compact JSON line per
/// regular member (including the manifest), sorted by UTF-8 path bytes.
#[must_use]
pub fn container_identity<'a>(
    members: impl IntoIterator<Item = (&'a str, u64, &'a str)>,
) -> String {
    let mut members = members.into_iter().collect::<Vec<_>>();
    members.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    let mut hasher = blake3::Hasher::new();
    for (path, size, digest) in members {
        let path = serde_json::to_string(path).expect("member path serializes");
        hasher.update(
            format!("{{\"path\":{path},\"size\":{size},\"digest\":\"{digest}\"}}\n").as_bytes(),
        );
    }
    tagged_blake3(hasher.finalize())
}

/// Canonical compact JSON with recursively sorted object keys, as used by
/// `parameters_digest`.
#[must_use]
pub fn canonical_json(value: &Value) -> Vec<u8> {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), sort(value)))
                    .collect(),
            ),
            Value::Array(array) => Value::Array(array.iter().map(sort).collect()),
            _ => value.clone(),
        }
    }
    serde_json::to_vec(&sort(value)).expect("JSON value serializes")
}

#[must_use]
pub fn blake3_digest(bytes: &[u8]) -> String {
    tagged_blake3(blake3::hash(bytes))
}

pub(crate) fn tagged_blake3(hash: blake3::Hash) -> String {
    format!("blake3:{hash}")
}

#[must_use]
pub fn is_blake3_digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(crate) fn error_diag(
    diagnostics: &mut Vec<ExchangeDiagnostic>,
    code: impl Into<String>,
    message: impl Into<String>,
    path: impl Into<String>,
) {
    diagnostics.push(ExchangeDiagnostic {
        severity: ExchangeDiagnosticSeverity::Error,
        code: code.into(),
        message: message.into(),
        path: Some(path.into()),
    });
}

pub(crate) fn warning_diag(
    diagnostics: &mut Vec<ExchangeDiagnostic>,
    code: impl Into<String>,
    message: impl Into<String>,
    path: impl Into<String>,
) {
    diagnostics.push(ExchangeDiagnostic {
        severity: ExchangeDiagnosticSeverity::Warning,
        code: code.into(),
        message: message.into(),
        path: Some(path.into()),
    });
}

pub(crate) fn sort_diagnostics(diagnostics: &mut [ExchangeDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
}

#[must_use]
pub fn has_errors(diagnostics: &[ExchangeDiagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|item| item.severity == ExchangeDiagnosticSeverity::Error)
}

/// Split a JSON Lines member into `(line number, value)` pairs. Every line,
/// including the last, must end in LF; blank lines are invalid.
pub(crate) fn parse_jsonl(
    text: &str,
    member: &str,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) -> Option<Vec<(usize, Value)>> {
    if !text.is_empty() && !text.ends_with('\n') {
        error_diag(
            diagnostics,
            "jsonl_invalid",
            "JSON Lines member must end with a line feed",
            member,
        );
        return None;
    }
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if line.trim().is_empty() {
            error_diag(
                diagnostics,
                "jsonl_invalid",
                format!("line {number}: blank lines are not allowed"),
                member,
            );
            return None;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => records.push((number, value)),
            Err(error) => {
                error_diag(
                    diagnostics,
                    "invalid_json",
                    format!("line {number}: {error}"),
                    member,
                );
                return None;
            }
        }
    }
    Some(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        let value = serde_json::json!({"b": 1, "a": {"d": [2, 1], "c": "é"}});
        assert_eq!(
            String::from_utf8(canonical_json(&value)).expect("utf8"),
            r#"{"a":{"c":"é","d":[2,1]},"b":1}"#
        );
    }

    #[test]
    fn selectors_serialize_without_unset_fields_or_integral_fractions() {
        let selectors = vec![
            ExchangeSelector::Interval {
                unit: "byte".to_string(),
                start: 74.0,
                end: 128.5,
                origin: Some(1.0),
                label_start: None,
                label_end: None,
            },
            ExchangeSelector::Fragment {
                value: "node".to_string(),
                conforms_to: None,
            },
        ];
        let json = serde_json::to_string(&selectors).expect("json");
        assert_eq!(
            json,
            r#"[{"type":"interval","unit":"byte","start":74,"end":128.5,"origin":1},{"type":"fragment","value":"node"}]"#
        );
        let round_trip: Vec<ExchangeSelector> = serde_json::from_str(&json).expect("parse");
        assert_eq!(round_trip, selectors);
        let yaml = serde_yaml::to_string(&selectors[0]).expect("yaml");
        assert!(
            yaml.contains("start: 74\n") && !yaml.contains("null"),
            "{yaml}"
        );
    }

    #[test]
    fn member_namespaces_are_reverse_domains() {
        assert!(valid_member_namespace("dev.tionis.renwiki"));
        assert!(!valid_member_namespace("renwiki"));
        assert!(!valid_member_namespace("dev..tionis"));
        assert!(!valid_member_namespace("dev.tionis/renwiki"));
        assert!(valid_namespace("dev.tionis.renwiki/node"));
        assert!(!valid_namespace("dev.tionis.renwiki"));
    }

    #[test]
    fn jsonl_rejects_missing_terminator_and_blank_lines() {
        let mut diagnostics = Vec::new();
        assert!(parse_jsonl("{}", "x.jsonl", &mut diagnostics).is_none());
        assert!(parse_jsonl("{}\n\n{}\n", "x.jsonl", &mut diagnostics).is_none());
        let parsed = parse_jsonl("{\"a\":1}\n{}\n", "x.jsonl", &mut Vec::new()).expect("parsed");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].0, 2);
    }

    #[test]
    fn provenance_requires_member_emission_and_acyclic_dependencies() {
        let digest = blake3_digest(&canonical_json(&serde_json::json!({})));
        let activity = |id: &str, depends: &[&str]| ExchangeActivity {
            id: id.to_string(),
            kind: "test".to_string(),
            started_at: None,
            ended_at: None,
            tools: vec![ExchangeTool {
                name: "t".to_string(),
                version: "1".to_string(),
                revision: None,
                package_url: None,
            }],
            models: Vec::new(),
            inputs: Vec::new(),
            outputs: vec!["a.md".to_string()],
            depends_on: depends.iter().map(ToString::to_string).collect(),
            parameters: serde_json::Map::new(),
            parameters_digest: digest.clone(),
        };
        let provenance = ExchangeProvenance {
            version: 1,
            activities: vec![activity("one", &["two"]), activity("two", &["one"])],
            redactions: Vec::new(),
        };
        let mut diagnostics = Vec::new();
        validate_provenance(
            &provenance,
            &[
                ProvenanceMember {
                    path: "a.md",
                    created_by: "one",
                },
                ProvenanceMember {
                    path: "b.md",
                    created_by: "one",
                },
            ],
            &BTreeSet::new(),
            "provenance.json",
            &mut diagnostics,
        );
        let codes = diagnostics
            .iter()
            .map(|item| item.code.as_str())
            .collect::<BTreeSet<_>>();
        assert!(codes.contains("activity_cycle"));
        assert!(codes.contains("member_provenance_invalid"));
    }
}
