//! Validator for Knowledge v1 JSON Lines snapshots.
//!
//! A knowledge snapshot is container-neutral: the embedding package supplies
//! the declared source table that evidence locators resolve against and,
//! optionally, the notes that entity and claim references may target.

use crate::exchange::{
    compile_schema, error_diag, parse_jsonl, validate_locator, validate_span, validate_with,
    ExchangeByteSpan, ExchangeDiagnostic, ExchangeSourceLocator,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const KNOWLEDGE_FORMAT: &str = "dev.tionis.knowledge";
pub const KNOWLEDGE_VERSION: u32 = 1;
const RECORD_SCHEMA: &str = include_str!("../resources/knowledge/v1/knowledge-record.schema.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeReview {
    Accepted,
    Unreviewed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeEvidenceRole {
    Support,
    Contradict,
    Context,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEvidence {
    pub role: KnowledgeEvidenceRole,
    pub locator: ExchangeSourceLocator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeEntity {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub review: KnowledgeReview,
    pub evidence: Vec<KnowledgeEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum KnowledgeObject {
    Entity { entity: String },
    Text { value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeAssertion {
    Attributed,
    Narrated,
    Inferred,
    Interpretation,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgePolarity {
    Positive,
    Negative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeScope {
    pub dimension: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeNoteReference {
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<ExchangeByteSpan>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeClaim {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: KnowledgeObject,
    pub assertion: KnowledgeAssertion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributed_to: Option<String>,
    pub polarity: KnowledgePolarity,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<KnowledgeScope>,
    pub review: KnowledgeReview,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub evidence: Vec<KnowledgeEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<KnowledgeNoteReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "lowercase")]
enum KnowledgeRecord {
    Header { format: String, version: u32 },
    Entity(KnowledgeEntity),
    Claim(KnowledgeClaim),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct KnowledgeSnapshot {
    pub entities: Vec<KnowledgeEntity>,
    pub claims: Vec<KnowledgeClaim>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct KnowledgeSummary {
    pub entities: usize,
    pub claims: usize,
    pub accepted_entities: usize,
    pub accepted_claims: usize,
}

impl KnowledgeSnapshot {
    #[must_use]
    pub fn summary(&self) -> KnowledgeSummary {
        KnowledgeSummary {
            entities: self.entities.len(),
            claims: self.claims.len(),
            accepted_entities: self
                .entities
                .iter()
                .filter(|entity| entity.review == KnowledgeReview::Accepted)
                .count(),
            accepted_claims: self
                .claims
                .iter()
                .filter(|claim| claim.review == KnowledgeReview::Accepted)
                .count(),
        }
    }
}

/// What the embedding container provides to a knowledge snapshot.
pub struct KnowledgeHost<'a> {
    pub source_ids: &'a BTreeSet<&'a str>,
    /// Declared note paths and their UTF-8 text. `None` means the snapshot
    /// is standalone, so note references cannot resolve.
    pub notes: Option<&'a BTreeMap<String, String>>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Header,
    Entities,
    Claims,
}

/// Validate a Knowledge v1 JSON Lines document. Returns the parsed snapshot
/// when the records could be read; semantic failures are diagnostics.
#[allow(clippy::too_many_lines)]
pub fn validate_knowledge_jsonl(
    text: &str,
    member: &str,
    host: &KnowledgeHost<'_>,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) -> Option<KnowledgeSnapshot> {
    let records = parse_jsonl(text, member, diagnostics)?;
    let validator = compile_schema(RECORD_SCHEMA);
    let mut snapshot = KnowledgeSnapshot::default();
    let mut phase = None;
    let mut previous_id: Option<String> = None;
    let mut ids = BTreeSet::new();
    for (line, value) in records {
        let before = diagnostics.len();
        validate_with(&validator, member, Some(line), &value, diagnostics);
        if diagnostics.len() != before {
            continue;
        }
        let record = match serde_json::from_value::<KnowledgeRecord>(value) {
            Ok(record) => record,
            Err(error) => {
                error_diag(
                    diagnostics,
                    "control_document_invalid",
                    format!("line {line}: {error}"),
                    member,
                );
                continue;
            }
        };
        let (record_phase, id) = match &record {
            KnowledgeRecord::Header { .. } => (Phase::Header, None),
            KnowledgeRecord::Entity(entity) => (Phase::Entities, Some(entity.id.clone())),
            KnowledgeRecord::Claim(claim) => (Phase::Claims, Some(claim.id.clone())),
        };
        match (phase, record_phase) {
            (None, Phase::Header) => {}
            (None, _) => error_diag(
                diagnostics,
                "knowledge_header_missing",
                "the first record must be the knowledge header",
                member,
            ),
            (Some(_), Phase::Header) => error_diag(
                diagnostics,
                "knowledge_header_duplicate",
                format!("line {line}: only the first record may be a header"),
                member,
            ),
            (Some(current), next) if next < current => error_diag(
                diagnostics,
                "record_order_invalid",
                format!("line {line}: entities must precede claims"),
                member,
            ),
            _ => {}
        }
        if phase != Some(record_phase) {
            previous_id = None;
        }
        phase = Some(phase.map_or(record_phase, |current| current.max(record_phase)));
        if let Some(id) = id {
            if previous_id
                .as_deref()
                .is_some_and(|previous| previous >= id.as_str())
            {
                error_diag(
                    diagnostics,
                    "record_order_invalid",
                    format!("line {line}: records must be sorted by strictly increasing id"),
                    member,
                );
            }
            if !ids.insert(id.clone()) {
                error_diag(
                    diagnostics,
                    "knowledge_id_duplicate",
                    format!("line {line}: duplicate id {id}"),
                    member,
                );
            }
            previous_id = Some(id);
        }
        match record {
            KnowledgeRecord::Header { .. } => {}
            KnowledgeRecord::Entity(entity) => snapshot.entities.push(entity),
            KnowledgeRecord::Claim(claim) => snapshot.claims.push(claim),
        }
    }
    if phase.is_none() {
        error_diag(
            diagnostics,
            "knowledge_header_missing",
            "the knowledge snapshot has no valid header record",
            member,
        );
    }
    validate_references(&snapshot, member, host, diagnostics);
    Some(snapshot)
}

fn validate_references(
    snapshot: &KnowledgeSnapshot,
    member: &str,
    host: &KnowledgeHost<'_>,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) {
    let entity_ids = snapshot
        .entities
        .iter()
        .map(|entity| entity.id.as_str())
        .collect::<BTreeSet<_>>();
    let check_entity = |id: &str, context: &str, diagnostics: &mut Vec<_>| {
        if !entity_ids.contains(id) {
            error_diag(
                diagnostics,
                "knowledge_entity_unknown",
                format!("{context} references unknown entity {id}"),
                member,
            );
        }
    };
    for entity in &snapshot.entities {
        for evidence in &entity.evidence {
            validate_locator(&evidence.locator, host.source_ids, member, diagnostics);
        }
        if let Some(note) = entity.note.as_deref() {
            validate_note_reference(note, None, host, member, diagnostics);
        }
    }
    for claim in &snapshot.claims {
        check_entity(&claim.subject, &claim.id, diagnostics);
        if let KnowledgeObject::Entity { entity } = &claim.object {
            check_entity(entity, &claim.id, diagnostics);
        }
        match (&claim.assertion, claim.attributed_to.as_deref()) {
            (KnowledgeAssertion::Attributed, Some(speaker)) => {
                check_entity(speaker, &claim.id, diagnostics);
            }
            (KnowledgeAssertion::Attributed, None) => error_diag(
                diagnostics,
                "claim_attribution_invalid",
                format!("{} is attributed but names no speaker", claim.id),
                member,
            ),
            (_, Some(_)) => error_diag(
                diagnostics,
                "claim_attribution_invalid",
                format!(
                    "{} names a speaker without an attributed assertion",
                    claim.id
                ),
                member,
            ),
            (_, None) => {}
        }
        if !claim
            .evidence
            .iter()
            .any(|evidence| evidence.role == KnowledgeEvidenceRole::Support)
        {
            error_diag(
                diagnostics,
                "claim_support_missing",
                format!("{} has no support evidence", claim.id),
                member,
            );
        }
        for evidence in &claim.evidence {
            validate_locator(&evidence.locator, host.source_ids, member, diagnostics);
        }
        for reference in &claim.notes {
            validate_note_reference(
                &reference.note,
                reference.document,
                host,
                member,
                diagnostics,
            );
        }
    }
}

fn validate_note_reference(
    note: &str,
    span: Option<ExchangeByteSpan>,
    host: &KnowledgeHost<'_>,
    member: &str,
    diagnostics: &mut Vec<ExchangeDiagnostic>,
) {
    let Some(notes) = host.notes else {
        error_diag(
            diagnostics,
            "knowledge_note_unbound",
            format!("{note} cannot resolve outside a wiki package"),
            member,
        );
        return;
    };
    match notes.get(note) {
        None => error_diag(
            diagnostics,
            "knowledge_note_unknown",
            format!("{note} is not a declared note"),
            member,
        ),
        Some(text) => {
            if let Some(span) = span {
                validate_span(span, text, member, diagnostics);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::has_errors;

    const HEADER: &str = r#"{"record":"header","format":"dev.tionis.knowledge","version":1}"#;

    fn evidence() -> &'static str {
        r#"[{"role":"support","locator":{"source_id":"game","selectors":[{"type":"fragment","value":"label-start"}]}}]"#
    }

    fn entity(id: &str) -> String {
        format!(
            r#"{{"record":"entity","id":"{id}","name":"{id}","type":"character","review":"accepted","evidence":{}}}"#,
            evidence()
        )
    }

    fn validate(text: &str, notes: Option<&BTreeMap<String, String>>) -> Vec<ExchangeDiagnostic> {
        let sources = BTreeSet::from(["game"]);
        let mut diagnostics = Vec::new();
        validate_knowledge_jsonl(
            text,
            "knowledge.jsonl",
            &KnowledgeHost {
                source_ids: &sources,
                notes,
            },
            &mut diagnostics,
        );
        diagnostics
    }

    fn codes(diagnostics: &[ExchangeDiagnostic]) -> BTreeSet<&str> {
        diagnostics.iter().map(|item| item.code.as_str()).collect()
    }

    #[test]
    fn accepts_sorted_entities_and_claims() {
        let claim = format!(
            r#"{{"record":"claim","id":"c1","subject":"alice","predicate":"knows","object":{{"kind":"entity","entity":"bob"}},"assertion":"attributed","attributed_to":"bob","polarity":"positive","scope":[{{"dimension":"route","value":"Bob"}}],"review":"unreviewed","evidence":{}}}"#,
            evidence()
        );
        let text = format!(
            "{HEADER}\n{}\n{}\n{claim}\n",
            entity("alice"),
            entity("bob")
        );
        let diagnostics = validate(&text, None);
        assert!(!has_errors(&diagnostics), "{diagnostics:?}");
    }

    #[test]
    fn rejects_order_unknown_entities_and_missing_support() {
        let claim = r#"{"record":"claim","id":"c1","subject":"zed","predicate":"is","object":{"kind":"text","value":"x"},"assertion":"narrated","polarity":"negative","review":"accepted","evidence":[{"role":"context","locator":{"source_id":"other","selectors":[]}}]}"#;
        let text = format!("{HEADER}\n{claim}\n{}\n{}\n", entity("b"), entity("a"));
        let diagnostics = validate(&text, None);
        let codes = codes(&diagnostics);
        for code in [
            "record_order_invalid",
            "knowledge_entity_unknown",
            "claim_support_missing",
            "locator_source_unknown",
        ] {
            assert!(codes.contains(code), "{code} missing from {codes:?}");
        }
    }

    #[test]
    fn note_references_require_a_host_note() {
        let with_note = format!(
            r#"{{"record":"entity","id":"a","name":"A","type":"dev.tionis.renwiki/route","note":"content/A.md","review":"accepted","evidence":{}}}"#,
            evidence()
        );
        let text = format!("{HEADER}\n{with_note}\n");
        assert!(codes(&validate(&text, None)).contains("knowledge_note_unbound"));
        let notes = BTreeMap::from([("content/B.md".to_string(), "# B\n".to_string())]);
        assert!(codes(&validate(&text, Some(&notes))).contains("knowledge_note_unknown"));
        let notes = BTreeMap::from([("content/A.md".to_string(), "# A\n".to_string())]);
        assert!(!has_errors(&validate(&text, Some(&notes))));
    }

    #[test]
    fn requires_header_and_rejects_unknown_entity_types() {
        let diagnostics = validate(&format!("{}\n", entity("a")), None);
        assert!(codes(&diagnostics).contains("knowledge_header_missing"));
        let bad_type = entity("a").replace("character", "wizard");
        let diagnostics = validate(&format!("{HEADER}\n{bad_type}\n"), None);
        assert!(codes(&diagnostics).contains("schema_violation"));
    }
}
