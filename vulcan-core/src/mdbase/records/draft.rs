use super::{
    record_body, record_diagnostic, sort_record_diagnostics, validate_record_schemas,
    yaml_mapping_to_json,
};
use crate::config::VaultConfig;
use crate::mdbase::{
    apply_mdbase_lifecycle_assignments, compose_mdbase_type_behavior, evaluate_mdbase_lifecycle,
    match_mdbase_record_types_with_context, MdbaseCelClock, MdbaseCelEngine, MdbaseCelLinkIndex,
    MdbaseCollection, MdbaseLifecycleEvent, MdbaseLifecycleInput, MdbaseLifecycleProviderError,
    MdbaseRecordDiagnostic, MdbaseRecordDiagnosticSeverity, MdbaseTypeRegistry,
    MdbaseValidationLevel,
};
use crate::parser::{parse_document, ParseDiagnosticKind};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub struct MdbaseRecordDraftRequest<'a> {
    pub path: &'a str,
    pub source: &'a str,
    pub old_source: Option<&'a str>,
    pub file: Value,
    pub operation: Value,
    pub clock: MdbaseCelClock,
    /// Must contain only dependencies whose visibility was proved by the caller.
    pub link_index: Option<Arc<MdbaseCelLinkIndex>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbasePreparedRecordDraft {
    pub source: String,
    pub frontmatter: Value,
    pub types: Vec<String>,
    pub generated_values: BTreeMap<String, Value>,
    pub diagnostics: Vec<MdbaseRecordDiagnostic>,
}

/// Prepare one authorized candidate: freeze membership, evaluate lifecycle once,
/// apply assignments, recheck membership exactly once, then validate schema.
/// This does not write any files. The caller must still validate collection-wide
/// constraints against the complete proposed final set before persisting.
pub fn prepare_mdbase_record_draft(
    collection: &MdbaseCollection,
    types: &MdbaseTypeRegistry,
    request: MdbaseRecordDraftRequest<'_>,
    engine: &MdbaseCelEngine,
    entropy: impl FnMut() -> Result<[u8; 16], MdbaseLifecycleProviderError>,
) -> Result<MdbasePreparedRecordDraft, Vec<MdbaseRecordDiagnostic>> {
    let frontmatter = parse_raw(request.source, request.path)?;
    let old = request
        .old_source
        .map(|source| parse_raw(source, request.path))
        .transpose()?;
    let body = record_body(request.source);
    let matched = match_mdbase_record_types_with_context(
        collection,
        types,
        request.path,
        &frontmatter,
        body,
        &request.clock,
    );
    check_match_diagnostics(&matched)?;
    let behavior = compose_mdbase_type_behavior(types, &matched.types);
    let known_fields = behavior
        .schemas
        .iter()
        .filter_map(|schema| schema.value.get("properties").and_then(Value::as_object))
        .flat_map(|properties| properties.keys().cloned())
        .collect();
    let lifecycle = evaluate_mdbase_lifecycle(
        &behavior,
        MdbaseLifecycleInput {
            event: if old.is_some() {
                MdbaseLifecycleEvent::Update
            } else {
                MdbaseLifecycleEvent::Create
            },
            draft: &frontmatter,
            old: old.as_ref(),
            file: request.file,
            operation: request.operation,
            known_fields,
            clock: request.clock.clone(),
            link_index: request.link_index,
        },
        engine,
        entropy,
    )
    .map_err(|errors| composition_errors(request.path, errors))?;
    let updated = apply_mdbase_lifecycle_assignments(&frontmatter, &lifecycle.assignments)
        .map_err(|error| composition_errors(request.path, vec![error]))?;
    let final_membership = match_mdbase_record_types_with_context(
        collection,
        types,
        request.path,
        &updated,
        body,
        &request.clock,
    );
    check_frozen_membership(&matched, &final_membership, request.path)?;
    let mut diagnostics = Vec::new();
    if collection.config.settings.validation != MdbaseValidationLevel::Off {
        validate_record_schemas(
            collection,
            types,
            request.path,
            &updated,
            &matched.types,
            &mut diagnostics,
        );
    }
    sort_record_diagnostics(&mut diagnostics);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == MdbaseRecordDiagnosticSeverity::Error)
    {
        return Err(diagnostics);
    }
    let source = if updated == frontmatter {
        request.source.to_string()
    } else {
        render_draft(request.source, &updated).map_err(|error| {
            vec![draft_error(
                request.path,
                "frontmatter_invalid",
                error.to_string(),
            )]
        })?
    };
    Ok(MdbasePreparedRecordDraft {
        source,
        frontmatter: updated,
        types: matched.types,
        generated_values: lifecycle.assignments,
        diagnostics,
    })
}

fn check_frozen_membership(
    before: &crate::mdbase::MdbaseTypeMatchResult,
    after: &crate::mdbase::MdbaseTypeMatchResult,
    path: &str,
) -> Result<(), Vec<MdbaseRecordDiagnostic>> {
    check_match_diagnostics(after)?;
    let membership = |names: &[String]| {
        names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>()
    };
    if membership(&after.types) != membership(&before.types) {
        return Err(vec![draft_error(
            path,
            "type_membership_changed",
            "lifecycle changed frozen type membership".to_string(),
        )]);
    }
    Ok(())
}

fn parse_raw(source: &str, path: &str) -> Result<Value, Vec<MdbaseRecordDiagnostic>> {
    let parsed = parse_document(
        source.strip_prefix('\u{feff}').unwrap_or(source),
        &VaultConfig::default(),
    );
    if let Some(diagnostic) = parsed
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == ParseDiagnosticKind::MalformedFrontmatter)
    {
        return Err(vec![draft_error(
            path,
            "frontmatter_invalid",
            diagnostic.message.clone(),
        )]);
    }
    if parsed.raw_frontmatter.is_none() {
        return Ok(serde_json::json!({}));
    }
    parsed
        .frontmatter
        .as_ref()
        .and_then(yaml_mapping_to_json)
        .ok_or_else(|| {
            vec![draft_error(
                path,
                "frontmatter_invalid",
                "frontmatter must be a YAML mapping".to_string(),
            )]
        })
}

fn draft_error(path: &str, code: &str, message: String) -> MdbaseRecordDiagnostic {
    record_diagnostic(
        MdbaseRecordDiagnosticSeverity::Error,
        code,
        message,
        path,
        "",
        None,
    )
}

fn check_match_diagnostics(
    matched: &crate::mdbase::MdbaseTypeMatchResult,
) -> Result<(), Vec<MdbaseRecordDiagnostic>> {
    if matched.diagnostics.is_empty() {
        return Ok(());
    }
    Err(matched
        .diagnostics
        .iter()
        .map(|diagnostic| {
            record_diagnostic(
                MdbaseRecordDiagnosticSeverity::Error,
                &diagnostic.code,
                diagnostic.message.clone(),
                &diagnostic.path,
                &diagnostic.field,
                diagnostic.type_name.clone(),
            )
        })
        .collect())
}

fn composition_errors(
    path: &str,
    errors: Vec<crate::mdbase::MdbaseTypeCompositionDiagnostic>,
) -> Vec<MdbaseRecordDiagnostic> {
    errors
        .into_iter()
        .map(|error| {
            let mut diagnostic = draft_error(path, &error.code, error.message);
            diagnostic.field = error.field;
            diagnostic.related_paths = error.locations;
            diagnostic
        })
        .collect()
}

fn render_draft(source: &str, frontmatter: &Value) -> Result<String, serde_yaml::Error> {
    let bom = if source.starts_with('\u{feff}') {
        "\u{feff}"
    } else {
        ""
    };
    let newline = if source
        .find('\n')
        .is_some_and(|index| source[..index].ends_with('\r'))
    {
        "\r\n"
    } else {
        "\n"
    };
    let yaml = serde_yaml::to_string(frontmatter)?;
    let yaml = yaml
        .strip_prefix("---\n")
        .unwrap_or(&yaml)
        .replace('\n', newline);
    let body = record_body(source);
    // Without original frontmatter record_body includes the initial BOM.
    let body = if body.len() == source.len() {
        body.strip_prefix('\u{feff}').unwrap_or(body)
    } else {
        body
    };
    Ok(format!("{bom}---{newline}{yaml}---{newline}{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::analyze_mdbase_record_source;
    use crate::mdbase::{load_mdbase_collection, load_mdbase_type_registry};
    use serde_json::json;
    use std::fs;

    fn fixture(
        policy: &str,
        schema: &str,
    ) -> (tempfile::TempDir, MdbaseCollection, MdbaseTypeRegistry) {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("mdbase.yaml"),
            "spec_version: 0.3.0\n",
        )
        .unwrap();
        fs::create_dir(directory.path().join("_types")).unwrap();
        fs::write(directory.path().join("_types/task.md"), format!("---\nkind: mdbase.type\nname: task\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value: {schema}\n{policy}\n---\n")).unwrap();
        let collection = load_mdbase_collection(directory.path()).unwrap().unwrap();
        let types = load_mdbase_type_registry(&collection).unwrap();
        assert!(types.diagnostics.is_empty(), "{:?}", types.diagnostics);
        (directory, collection, types)
    }

    fn prepare(
        collection: &MdbaseCollection,
        types: &MdbaseTypeRegistry,
        source: &str,
        old_source: Option<&str>,
    ) -> Result<MdbasePreparedRecordDraft, Vec<MdbaseRecordDiagnostic>> {
        prepare_mdbase_record_draft(
            collection,
            types,
            MdbaseRecordDraftRequest {
                path: "tasks/new.md",
                source,
                old_source,
                file: json!({"path": "tasks/new.md"}),
                operation: json!({"kind": if old_source.is_some() { "update" } else { "create" }}),
                clock: MdbaseCelClock::new(
                    "2026-09-08T23:30:45Z".parse().unwrap(),
                    "Europe/Berlin",
                )
                .unwrap(),
                link_index: None,
            },
            &MdbaseCelEngine::default(),
            || Ok([0; 16]),
        )
    }

    #[test]
    fn generated_required_values_are_validated_after_lifecycle_without_writing() {
        let (directory, collection, types) = fixture("collection:\n  read_defaults: {status: open}\nlifecycle:\n  on_create:\n    set:\n      id: {ulid: true}\n      created: {now: true}", "{type: object, required: [id, created], properties: {id: {type: string}, created: {type: string, format: date-time}}}");
        let source = "---\ntype: task\n---\n# Body\n[[Other|Alias]]\n";
        assert!(
            !analyze_mdbase_record_source(&collection, &types, "tasks/new.md", source).is_valid()
        );
        let prepared = prepare(&collection, &types, source, None).unwrap();
        assert!(prepared.diagnostics.is_empty());
        assert!(prepared.frontmatter["id"].is_string());
        assert_eq!(prepared.frontmatter["created"], "2026-09-08T23:30:45.000Z");
        assert!(prepared.frontmatter.get("status").is_none());
        assert_eq!(record_body(&prepared.source), "# Body\n[[Other|Alias]]\n");
        assert!(!directory.path().join("tasks/new.md").exists());
    }

    #[test]
    fn schema_rejects_an_invalid_generated_value() {
        let (_directory, collection, types) = fixture(
            "lifecycle:\n  on_create:\n    set:\n      count: {literal: bad}",
            "{type: object, properties: {count: {type: integer}}}",
        );
        let errors = prepare(&collection, &types, "---\ntype: task\n---\nBody", None).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.field == "/count" && error.code.starts_with("schema_")),
            "{errors:?}"
        );
    }

    #[test]
    fn unchanged_policy_preserves_exact_source_and_changed_policy_preserves_body_and_line_endings()
    {
        let (_directory, collection, types) = fixture(
            "lifecycle:\n  on_update:\n    set:\n      status: {literal: done}",
            "{type: object}",
        );
        let old = "---\ntype: task\nstatus: open\n---\n";
        let source = "\u{feff}---\r\n# keep comment\r\ntype: 'task'\r\nstatus: done\r\n...\r\n[[Other#Heading|Alias]]\r\nBody  \r\n";
        let unchanged = prepare(&collection, &types, source, Some(old)).unwrap();
        assert_eq!(unchanged.source, source);
        let changed_source = source.replace("status: done", "status: open");
        let changed = prepare(&collection, &types, &changed_source, Some(old)).unwrap();
        assert!(changed.source.starts_with("\u{feff}---\r\n"));
        assert_eq!(record_body(&changed.source), record_body(source));
        assert!(!changed.source.replace("\r\n", "").contains('\n'));
        assert_eq!(
            parse_raw(&changed.source, "tasks/new.md").unwrap(),
            changed.frontmatter
        );
    }

    #[test]
    fn lifecycle_cannot_add_inferred_membership_or_remove_explicit_membership() {
        let (directory, collection, _) = fixture(
            "lifecycle:\n  on_create:\n    set:\n      marker: {literal: yes}",
            "{type: object}",
        );
        fs::write(directory.path().join("_types/extra.md"), "---\nkind: mdbase.type\nname: extra\nversion: 1\nmatch:\n  fields_present: [marker]\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\n---\n").unwrap();
        // Inferred mode matches task on title and extra only after lifecycle.
        let task = fs::read_to_string(directory.path().join("_types/task.md"))
            .unwrap()
            .replace(
                "version: 1",
                "version: 1\nmatch:\n  fields_present: [title]",
            );
        fs::write(directory.path().join("_types/task.md"), task).unwrap();
        let types = load_mdbase_type_registry(&collection).unwrap();
        let error = prepare(&collection, &types, "---\ntitle: New\n---\n", None).unwrap_err();
        assert_eq!(error[0].code, "type_membership_changed");

        let (_directory, collection, types) = fixture(
            "lifecycle:\n  on_create:\n    set:\n      type: {literal: []}",
            "{type: object}",
        );
        assert!(prepare(&collection, &types, "---\ntype: task\n---\n", None).is_err());
    }

    #[test]
    fn create_without_frontmatter_adds_it_once_and_preserves_bom_body() {
        let (directory, collection, _) = fixture("match:\n  path_glob: 'tasks/**'\nlifecycle:\n  on_create:\n    set:\n      title: {literal: New}", "{type: object}");
        let types = load_mdbase_type_registry(&collection).unwrap();
        let source = "\u{feff}# Heading\r\nBody\r\n";
        let prepared = prepare(&collection, &types, source, None).unwrap();
        assert_eq!(prepared.source.matches('\u{feff}').count(), 1);
        assert_eq!(record_body(&prepared.source), "# Heading\r\nBody\r\n");
        assert_eq!(prepared.frontmatter["title"], "New");
        assert!(!directory.path().join("tasks").exists());
    }

    #[test]
    fn membership_is_a_set_and_does_not_reject_reordered_explicit_types() {
        let (directory, collection, _) = fixture(
            "lifecycle:\n  on_create:\n    set:\n      type: {literal: [extra, task]}",
            "{type: object}",
        );
        fs::write(directory.path().join("_types/extra.md"), "---\nkind: mdbase.type\nname: extra\nversion: 1\nschema:\n  dialect: json-schema-2020-12\n  value: {type: object}\n---\n").unwrap();
        let types = load_mdbase_type_registry(&collection).unwrap();
        let prepared =
            prepare(&collection, &types, "---\ntype: [task, extra]\n---\n", None).unwrap();
        assert_eq!(prepared.frontmatter["type"], json!(["extra", "task"]));
    }

    #[test]
    fn adding_frontmatter_never_discards_a_leading_unclosed_thematic_break() {
        let (_directory, collection, types) = fixture("match:\n  path_glob: 'tasks/**'\nlifecycle:\n  on_create:\n    set:\n      title: {literal: New}", "{type: object}");
        let source = "---\n# Heading\nBody that must survive\n";
        assert_eq!(record_body(source), source);
        let prepared = prepare(&collection, &types, source, None).unwrap();
        assert_eq!(record_body(&prepared.source), source);
    }

    #[test]
    fn validation_policy_retains_warnings_but_never_accepts_malformed_frontmatter() {
        let (_directory, mut collection, types) = fixture("", "{type: object, required: [title]}");
        collection.config.settings.validation = MdbaseValidationLevel::Warn;
        let prepared = prepare(&collection, &types, "---\ntype: task\n---\n", None).unwrap();
        assert!(!prepared.diagnostics.is_empty());
        assert!(prepared
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity == MdbaseRecordDiagnosticSeverity::Warning));
        collection.config.settings.validation = MdbaseValidationLevel::Off;
        assert!(prepare(&collection, &types, "---\ntype: task\n---\n", None)
            .unwrap()
            .diagnostics
            .is_empty());
        for source in ["---\na: [\n---\n", "---\n- item\n---\n"] {
            let errors = prepare(&collection, &types, source, None).unwrap_err();
            assert_eq!(errors[0].code, "frontmatter_invalid");
        }
    }
}
