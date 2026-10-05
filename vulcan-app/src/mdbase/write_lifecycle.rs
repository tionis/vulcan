use super::{
    write_validation, AppError, LoadedCollection, MdbaseManagedWriteMode, MdbaseWriteOperation,
    MdbaseWritePreview, MdbaseWritePreviewRequest,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use vulcan_core::mdbase::{
    analyze_mdbase_record_set_sources, analyze_mdbase_record_source_with_clock,
    compose_mdbase_type_behavior, is_mdbase_record_path, mdbase_cel_file_value,
    mdbase_lifecycle_requires_link_index, prepare_mdbase_record_draft, MdbaseCelClock,
    MdbaseCelEngine, MdbaseCelLinkIndex, MdbaseLifecycleProviderError, MdbaseRecordDiagnostic,
    MdbaseRecordDraftRequest, MdbaseRecordSet,
};
use vulcan_core::paths::secure_open_read;

#[cfg(test)]
mod tests;

pub(super) fn prepare_preview(
    loaded: &LoadedCollection,
    scoped: MdbaseWritePreview,
    mut request: MdbaseWritePreviewRequest,
    operation: &MdbaseWriteOperation,
    clock: &MdbaseCelClock,
    mode: MdbaseManagedWriteMode,
) -> Result<MdbaseWritePreview, AppError> {
    if mode == MdbaseManagedWriteMode::RawRepair {
        return Ok(scoped);
    }
    reject_optional_events(loaded, &scoped, operation, clock)?;
    let mut sources = write_validation::proposed_sources(loaded, &scoped)?;
    // Only lifecycle evaluation reads the collection-resolved file value. When
    // no affected type declares lifecycle actions, the drafts need no other
    // records, so the rest of the collection is not analyzed here.
    let lifecycle = loaded.types.iter().any(|definition| {
        definition.frontmatter.get("lifecycle").is_some()
            && scoped
                .matched_types
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&definition.name))
    });
    if !lifecycle {
        sources.retain(|path, _| scoped.changes.iter().any(|change| change.path == *path));
    }
    let mut records =
        analyze_mdbase_record_set_sources(&loaded.collection, &loaded.types, &sources, clock);
    add_existing_metadata(loaded, &scoped, &mut records)?;
    let index =
        mdbase_lifecycle_requires_link_index(&loaded.types, &scoped.matched_types).then(|| {
            Arc::new(MdbaseCelLinkIndex::new(
                &records,
                &loaded.collection.config.settings.id_field,
            ))
        });
    let engine = MdbaseCelEngine::default();
    let mut generated = BTreeMap::new();
    for change in &mut request.changes {
        let Some(source) = &change.after else {
            continue;
        };
        if matches!(operation, MdbaseWriteOperation::Rename { to, .. } if *to == change.path) {
            // Rename has its own optional event, never on_create. In particular
            // moving a note must not regenerate its identity or creation date.
            continue;
        }
        let Some(record) = records.get(&change.path) else {
            continue;
        };
        let before = scoped
            .changes
            .iter()
            .find(|candidate| candidate.path == change.path)
            .and_then(|candidate| candidate.before.as_deref());
        let prepared = prepare_mdbase_record_draft(
            &loaded.collection,
            &loaded.types,
            MdbaseRecordDraftRequest {
                path: &change.path,
                source,
                old_source: before,
                file: mdbase_cel_file_value(record),
                operation: json!({
                    "kind": if before.is_some() { "update" } else { "create" },
                    "parent_kind": operation.name(),
                    "path": change.path,
                }),
                clock: clock.clone(),
                link_index: index.clone(),
            },
            &engine,
            fresh_entropy,
        )
        .map_err(|diagnostics| draft_error(&diagnostics))?;
        if !prepared.generated_values.is_empty() {
            generated.insert(change.path.clone(), json!(prepared.generated_values));
        }
        change.after = Some(prepared.source);
    }
    if generated.is_empty() {
        return Ok(scoped);
    }
    request.generated_values = generated;
    let prepared = loaded.capture_preview(request)?;
    write_validation::check_snapshot_stability(&scoped, &prepared)?;
    if scoped.accepted_revisions != prepared.accepted_revisions
        || scoped.directory_memberships != prepared.directory_memberships
    {
        return Err(AppError::operation_with_code(
            "stale_state",
            "mdbase lifecycle dependencies changed while planning",
        ));
    }
    Ok(prepared)
}

fn draft_error(diagnostics: &[MdbaseRecordDiagnostic]) -> AppError {
    let code = diagnostics
        .first()
        .map_or("validation_failed", |diagnostic| {
            let code = diagnostic.code.as_str();
            if code.starts_with("lifecycle_")
                || matches!(code, "type_membership_changed" | "type_conflict")
            {
                code
            } else {
                "validation_failed"
            }
        });
    AppError::operation_with_code(
        code,
        format!(
            "mdbase validation rejected the managed note write: {}",
            super::validation_error_summary(diagnostics),
        ),
    )
}

fn fresh_entropy() -> Result<[u8; 16], MdbaseLifecycleProviderError> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|error| MdbaseLifecycleProviderError {
        code: "lifecycle_provider_error".to_string(),
        message: format!("cannot obtain lifecycle entropy: {error}"),
    })?;
    Ok(bytes)
}

fn add_existing_metadata(
    loaded: &LoadedCollection,
    scoped: &MdbaseWritePreview,
    records: &mut MdbaseRecordSet,
) -> Result<(), AppError> {
    for record in &mut records.records {
        if !scoped.accepted_revisions.contains_key(&record.path) {
            continue;
        }
        let metadata = secure_open_read(&loaded.collection.root, Path::new(&record.path))
            .and_then(|file| file.metadata())
            .map_err(|_| {
                AppError::operation_with_code(
                    "stale_state",
                    "mdbase lifecycle metadata changed while planning",
                )
            })?;
        let timestamp =
            |time| DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true);
        record.file.mtime = metadata.modified().ok().map(timestamp);
        record.file.ctime = metadata.created().ok().map(timestamp);
    }
    Ok(())
}

fn reject_optional_events(
    loaded: &LoadedCollection,
    preview: &MdbaseWritePreview,
    operation: &MdbaseWriteOperation,
    clock: &MdbaseCelClock,
) -> Result<(), AppError> {
    for change in &preview.changes {
        let event = if matches!(operation, MdbaseWriteOperation::Rename { from, to } if *from == change.path || *to == change.path)
        {
            "on_rename"
        } else if change.after.is_none() {
            "on_delete"
        } else {
            continue;
        };
        if !is_mdbase_record_path(&loaded.collection, &change.path).map_err(AppError::operation)? {
            continue;
        }
        for source in change.before.iter().chain(change.after.iter()) {
            let analysis = analyze_mdbase_record_source_with_clock(
                &loaded.collection,
                &loaded.types,
                &change.path,
                source,
                clock,
            );
            let behavior = compose_mdbase_type_behavior(&loaded.types, &analysis.types);
            if behavior.lifecycle.contains_key(event) {
                return Err(AppError::operation_with_code(
                    "lifecycle_event_unsupported",
                    format!("mdbase {event} is not supported; the write was not performed"),
                ));
            }
        }
    }
    Ok(())
}
