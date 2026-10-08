//! Inspecting and recovering an interrupted mdbase write under an explicit
//! permission profile.
//!
//! Recovery acts with the caller's own authority, never a borrowed one: the
//! profile must read the collection controls (as any managed write does) and
//! read and write every path the pending transaction touches. A caller
//! lacking any of them gets a generic denial that names no pending path.
//! Recovery rolls the transaction back or forward exactly as the journal
//! decides; a transaction recovery cannot complete because files were edited
//! outside it is retired only by an explicit, review-token-bound decision.

use super::{reconcile_committed_write, write_control_filter, LoadedCollection};
use crate::AppError;
use serde::Serialize;
use vulcan_core::mdbase::{
    accept_current_mdbase_write_transaction, inspect_mdbase_write_transaction,
    load_mdbase_collection, recover_mdbase_write_transaction, MdbaseWriteAcceptCurrentOutcome,
    MdbaseWriteOutcome, MdbaseWriteReview,
};
use vulcan_core::{
    resolve_permission_profile, PermissionGuard, ProfilePermissionGuard, ScanSummary, VaultPaths,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseWriteRepairStatus {
    pub pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<MdbaseWriteReview>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseWriteRecoveryReport {
    pub dry_run: bool,
    /// The transaction as found, before any recovery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<MdbaseWriteReview>,
    /// The committed outcome when recovery rolled forward or finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<MdbaseWriteOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MdbaseWriteAcceptCurrentReport {
    pub accepted: MdbaseWriteAcceptCurrentOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanSummary>,
}

/// The pending transaction, if any, once `permission_profile` is proved to
/// cover it.
pub fn build_mdbase_write_repair_status(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
) -> Result<MdbaseWriteRepairStatus, AppError> {
    let (_, loaded) = authorized_collection(paths, permission_profile)?;
    let review = authorized_review(paths, &loaded, permission_profile)?;
    Ok(MdbaseWriteRepairStatus {
        pending: review.is_some(),
        review,
    })
}

/// Recover a pending transaction as its journal decides: roll back before
/// the commit decision, roll forward after it, then publish the record cache
/// and index the touched notes. A dry run only reports what was found.
pub fn recover_mdbase_write(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    dry_run: bool,
) -> Result<MdbaseWriteRecoveryReport, AppError> {
    let (filter, loaded) = authorized_collection(paths, permission_profile)?;
    let review = authorized_review(paths, &loaded, permission_profile)?;
    if dry_run || review.is_none() {
        return Ok(MdbaseWriteRecoveryReport {
            dry_run,
            review,
            outcome: None,
            scan: None,
        });
    }
    let mut scan = None;
    let outcome = recover_mdbase_write_transaction(paths, &loaded.collection, |event| {
        // The journal's operation is known only by name here; a rename keeps
        // no identity hint, so the moved note is indexed afresh.
        scan = Some(reconcile_committed_write(
            paths,
            &loaded,
            &filter,
            event,
            None,
            &mut super::ReconcileStats::default(),
        )?);
        Ok(())
    })
    .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    if scan.is_none() {
        // A rolled-back transaction changed files back; index them again.
        scan = Some(
            vulcan_core::scan_vault(paths, vulcan_core::ScanMode::Incremental)
                .map_err(AppError::operation)?,
        );
    }
    Ok(MdbaseWriteRecoveryReport {
        dry_run,
        review,
        outcome,
        scan,
    })
}

/// Retire a pending transaction that recovery cannot complete, keeping the
/// current files, after a person reconciled them against `review_token`.
pub fn accept_current_mdbase_write(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
    transaction_id: &str,
    review_token: &str,
    dry_run: bool,
) -> Result<MdbaseWriteAcceptCurrentReport, AppError> {
    let (_, loaded) = authorized_collection(paths, permission_profile)?;
    authorized_review(paths, &loaded, permission_profile)?;
    let accepted = accept_current_mdbase_write_transaction(
        paths,
        &loaded.collection,
        transaction_id,
        review_token,
        dry_run,
    )
    .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    let scan = (!dry_run)
        .then(|| vulcan_core::scan_vault(paths, vulcan_core::ScanMode::Incremental))
        .transpose()
        .map_err(AppError::operation)?;
    Ok(MdbaseWriteAcceptCurrentReport { accepted, scan })
}

/// The collection and its controls under the profile's control authority,
/// without the consistent-read guard a pending journal would refuse.
fn authorized_collection(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
) -> Result<(vulcan_core::PermissionFilter, LoadedCollection), AppError> {
    let guard = profile_guard(paths, permission_profile)?;
    let filter = write_control_filter(&guard)?;
    let collection = load_mdbase_collection(paths.vault_root())
        .map_err(AppError::operation)?
        .ok_or_else(|| AppError::operation("not an mdbase collection: missing mdbase.yaml"))?;
    let (types, contracts) = super::load_control_registries(&collection, Some(&filter))?;
    Ok((
        filter.clone(),
        LoadedCollection {
            read_guard: None,
            control_filter: Some(filter),
            collection,
            types,
            contracts,
            known_revisions: None,
            cached_local: None,
        },
    ))
}

fn profile_guard(
    paths: &VaultPaths,
    permission_profile: Option<&str>,
) -> Result<ProfilePermissionGuard, AppError> {
    let selection =
        resolve_permission_profile(paths, permission_profile).map_err(AppError::operation)?;
    Ok(ProfilePermissionGuard::new(paths, selection))
}

/// The pending review, only when the profile can read and write every path
/// it touches. A denial does not name the path.
fn authorized_review(
    paths: &VaultPaths,
    loaded: &LoadedCollection,
    permission_profile: Option<&str>,
) -> Result<Option<MdbaseWriteReview>, AppError> {
    let guard = profile_guard(paths, permission_profile)?;
    let review = inspect_mdbase_write_transaction(paths, &loaded.collection)
        .map_err(|error| AppError::operation_with_code(error.code, error.message))?;
    if let Some(review) = review.as_ref() {
        for change in &review.changes {
            if guard.check_read_path(&change.path).is_err()
                || guard.check_write_path(&change.path).is_err()
            {
                return Err(AppError::operation_with_code(
                    "permission_denied",
                    "the selected profile cannot recover the pending mdbase write",
                ));
            }
        }
    }
    Ok(review)
}

/// Commit an update to `path` whose post-commit reconciliation fails,
/// leaving its journal pending exactly as a process stopped before
/// reconciling would. Used by the recovery conformance gate and tests.
pub(super) fn interrupt_update_for_gate(
    paths: &VaultPaths,
    path: &str,
    after: &str,
) -> Result<(), String> {
    use super::{
        config_revision, load_write_config, permission_revision, plan_mdbase_write,
        MdbaseWriteChangeRequest, MdbaseWriteOperation, MdbaseWritePlanRequest,
    };
    use vulcan_core::mdbase::{
        apply_mdbase_write_transaction_with_control_filter, MdbaseWriteApplyRequest,
        MdbaseWritePreviewVerification,
    };
    let text = |error: AppError| error.to_string();
    let now = chrono::Utc::now();
    let plan = plan_mdbase_write(
        paths,
        &MdbaseWritePlanRequest {
            caller_id: "interrupt".to_string(),
            instance_id: "interrupt".to_string(),
            operation: MdbaseWriteOperation::Update,
            changes: vec![MdbaseWriteChangeRequest {
                path: path.to_string(),
                after: Some(after.to_string()),
                if_revision: None,
            }],
            matched_types: Vec::new(),
            generated_values: std::collections::BTreeMap::new(),
            permission_profile: None,
            ttl_seconds: Some(300),
        },
        now,
    )
    .map_err(text)?;
    let guard = profile_guard(paths, None).map_err(text)?;
    let (filter, loaded) = authorized_collection(paths, None).map_err(text)?;
    let permission = permission_revision(guard.selection()).map_err(text)?;
    let config = config_revision(&load_write_config(paths).map_err(text)?).map_err(text)?;
    let outcome = apply_mdbase_write_transaction_with_control_filter(
        paths,
        &loaded.collection,
        &MdbaseWriteApplyRequest {
            preview: &plan.preview,
            verification: MdbaseWritePreviewVerification {
                caller_id: "interrupt",
                instance_id: "interrupt",
                operation: &plan.preview.operation,
                permission_revision: &permission,
                config_revision: &config,
                now,
                known_revisions: None,
            },
            idempotency_key: "interrupted",
        },
        Some(&filter),
        || Ok(()),
        |_| Err("process stopped before reconciling".to_string()),
    )
    .map_err(|error| error.message)?;
    if outcome.follow_up_error.is_none() {
        return Err("the write was reconciled".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdbase::tests::fixture;
    use std::fs;

    fn interrupted_update(paths: &VaultPaths) {
        interrupt_update_for_gate(
            paths,
            "tasks/public.md",
            "---\ntype: task\ntitle: Recovered\n---\nBody\n",
        )
        .unwrap();
    }

    #[test]
    fn interrupted_writes_recover_under_an_authorized_profile_only() {
        let (directory, paths) = fixture();
        vulcan_core::initialize_vulcan_dir(&paths).unwrap();
        vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Full).unwrap();
        interrupted_update(&paths);
        assert_eq!(
            vulcan_core::mdbase::acquire_mdbase_consistent_read(&paths)
                .unwrap_err()
                .code,
            "recovery_required"
        );

        // A profile that cannot write the pending path learns nothing.
        fs::write(
            paths.config_file(),
            "[permissions.profiles.reader]\nread = \"all\"\nwrite = \"none\"\n",
        )
        .unwrap();
        for result in [
            build_mdbase_write_repair_status(&paths, Some("reader")).map(|_| ()),
            recover_mdbase_write(&paths, Some("reader"), false).map(|_| ()),
        ] {
            let error = result.unwrap_err();
            assert_eq!(error.code(), Some("permission_denied"));
            assert!(!error.to_string().contains("tasks/public.md"));
        }

        let status = build_mdbase_write_repair_status(&paths, None).unwrap();
        let review = status.review.expect("pending review");
        assert_eq!(review.recovery, "roll_forward");
        assert!(review.recoverable);
        assert_eq!(review.changes[0].state, "after");

        let preview = recover_mdbase_write(&paths, None, true).unwrap();
        assert!(preview.outcome.is_none());
        assert!(
            build_mdbase_write_repair_status(&paths, None)
                .unwrap()
                .pending
        );

        let recovered = recover_mdbase_write(&paths, None, false).unwrap();
        assert!(recovered.outcome.is_some());
        assert!(recovered.scan.is_some());
        assert!(
            !build_mdbase_write_repair_status(&paths, None)
                .unwrap()
                .pending
        );
        let read = crate::mdbase::build_mdbase_read_report(&paths, "tasks/public.md", false, None)
            .unwrap();
        assert_eq!(read.record.frontmatter["title"], "Recovered");
        assert!(fs::read_to_string(directory.path().join("tasks/public.md"))
            .unwrap()
            .contains("Recovered"));
    }
}
