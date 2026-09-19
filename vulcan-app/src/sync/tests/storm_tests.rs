use super::*;
use crate::sync_conflicts::{
    conflict_groups, get_sync_conflict_page_with_state_store, get_sync_conflict_with_state_store,
    list_sync_conflicts_with_state_store, resolve_sync_conflict_with_state_store,
    ResolveSyncConflictOptions, SyncConflictResolutionSide, SyncConflictResolutionState,
};
use std::collections::BTreeSet;

const FORMATTING_PERCENT: usize = 80;

fn conflict_storm_fixture(path_count: usize) -> StructuredSyncFixture {
    let owned = (0..path_count)
        .map(|index| {
            (
                format!("notes/note-{index:05}.md"),
                format!("# Note {index}\n\nvalue {index}\n"),
            )
        })
        .chain([
            ("data.json".to_string(), "{\"base\":true}\n".to_string()),
            ("writer-clean.md".to_string(), "writer base\n".to_string()),
            ("reader-clean.md".to_string(), "reader base\n".to_string()),
        ])
        .collect::<Vec<_>>();
    let borrowed = owned
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_str()))
        .collect::<Vec<_>>();
    structured_sync_fixture(&borrowed)
}

fn apply_storm_edits(fixture: &StructuredSyncFixture, path_count: usize) -> usize {
    let formatting_count = path_count * FORMATTING_PERCENT / 100;
    for index in 0..path_count {
        let path = format!("notes/note-{index:05}.md");
        let (writer, reader) = if index < formatting_count {
            (
                format!("# Note {index}\n\nvalue  {index}\n"),
                format!("# Note {index}\n\nvalue\t{index}\n"),
            )
        } else {
            (
                format!("# Note {index}\n\nwriter {index}\n"),
                format!("# Note {index}\n\nreader {index}\n"),
            )
        };
        fs::write(fixture.writer.join(&path), writer).expect("writer storm edit");
        fs::write(fixture.reader.join(path), reader).expect("reader storm edit");
    }
    fs::write(
        fixture.writer.join("data.json"),
        "{\"base\":true,\"writer\":1}\n",
    )
    .expect("writer JSON edit");
    fs::write(
        fixture.reader.join("data.json"),
        "{\"base\":true,\"reader\":2}\n",
    )
    .expect("reader JSON edit");
    fs::write(fixture.writer.join("writer-clean.md"), "writer clean\n").expect("writer clean edit");
    fs::write(fixture.reader.join("reader-clean.md"), "reader clean\n").expect("reader clean edit");
    formatting_count
}

fn resolution_options(group_ids: &[String], dry_run: bool) -> ResolveSyncConflictOptions {
    ResolveSyncConflictOptions {
        side: SyncConflictResolutionSide::Local,
        group_ids: group_ids.to_vec(),
        remote: vulcan_sync::GitRemote::parse("origin").expect("remote"),
        live_ref: vulcan_sync::GitRefName::parse("refs/heads/__vulcan-sync/live")
            .expect("live ref"),
        dry_run,
    }
}

fn assert_bounded_pages(
    fixture: &StructuredSyncFixture,
    store: &SyncStateStore,
    conflict_id: &str,
    path_count: usize,
) {
    let mut offset = 0;
    let mut paths = BTreeSet::new();
    let mut groups = BTreeSet::new();
    loop {
        let detail = get_sync_conflict_page_with_state_store(
            &VaultPaths::new(&fixture.reader),
            conflict_id,
            offset,
            256,
            store,
        )
        .expect("bounded conflict page");
        let page = detail.path_page.expect("page metadata");
        assert_eq!(page.total, path_count);
        assert!(detail.record.paths.len() <= 256);
        for path in detail.record.paths {
            assert!(paths.insert(path.path), "duplicate paged path");
            assert!(groups.insert(path.group_id), "duplicate singleton group");
        }
        let Some(next) = page.next_offset else {
            break;
        };
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(paths.len(), path_count);
    assert_eq!(groups.len(), path_count);
}

fn resolve_group_batch(
    fixture: &StructuredSyncFixture,
    store: &SyncStateStore,
    conflict_id: &str,
    group_ids: &[String],
) {
    resolve_sync_conflict_with_state_store(
        &VaultPaths::new(&fixture.reader),
        conflict_id,
        &resolution_options(group_ids, false),
        store,
    )
    .expect("resolve storm group batch");
}

fn assert_final_storm_tree(fixture: &StructuredSyncFixture, path_count: usize) {
    for index in 0..path_count {
        let expected = if index < path_count * FORMATTING_PERCENT / 100 {
            format!("# Note {index}\n\nvalue\t{index}\n")
        } else {
            format!("# Note {index}\n\nreader {index}\n")
        };
        assert_eq!(
            fs::read_to_string(fixture.reader.join(format!("notes/note-{index:05}.md")))
                .expect("resolved note"),
            expected
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.reader.join("writer-clean.md")).expect("writer clean path"),
        "writer clean\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.reader.join("reader-clean.md")).expect("reader clean path"),
        "reader clean\n"
    );
    let json: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(fixture.reader.join("data.json")).expect("merged JSON"),
    )
    .expect("valid merged JSON");
    assert_eq!(json["base"], true);
    assert_eq!(json["writer"], 1);
    assert_eq!(json["reader"], 2);
    assert_eq!(
        fs::read_to_string(fixture.reader.join("unrelated.md")).expect("unrelated advance"),
        "unrelated advancement\n"
    );
}

#[allow(clippy::too_many_lines)] // Keep the scale lifecycle as one ordered acceptance scenario.
fn run_conflict_storm(path_count: usize, finish_all_groups: bool) {
    let fixture = conflict_storm_fixture(path_count);
    let normal_index_before = git_stdout(&fixture.reader, &["ls-files", "--stage", "-z"]);
    let formatting_count = apply_storm_edits(&fixture, path_count);
    sync_git_vault_with_state_store(
        &VaultPaths::new(&fixture.writer),
        &GitSyncOptions::default(),
        &fixture.store,
    )
    .expect("writer storm push");
    let report = sync_git_vault_with_state_store(
        &VaultPaths::new(&fixture.reader),
        &GitSyncOptions::default(),
        &fixture.store,
    )
    .expect("reader storm conflict");

    assert_eq!(report.sync.outcome, GitSyncOutcome::Conflicted);
    assert_eq!(report.operational_stats.conflict_paths, path_count);
    assert_eq!(report.operational_stats.conflict_groups, path_count);
    assert_eq!(
        report.operational_stats.formatting_candidate_paths,
        formatting_count
    );
    assert_eq!(report.operational_stats.automatic_resolution_paths, 1);
    let git_subprocesses = report
        .operational_stats
        .git_subprocesses
        .expect("CLI subprocess metrics");
    assert!(
        git_subprocesses <= 160,
        "initial conflict cycle used {git_subprocesses} Git subprocesses"
    );
    let encoded = serde_json::to_vec(&report).expect("serialize actual storm report");
    assert!(
        encoded.len() < 96 * 1024,
        "report was {} bytes",
        encoded.len()
    );
    let json: serde_json::Value = serde_json::from_slice(&encoded).expect("report JSON");
    assert_eq!(json["conflict"]["path_count"], path_count);
    assert_eq!(
        json["conflict"]["paths"].as_array().expect("paths").len(),
        16
    );
    assert_eq!(json["conflict"]["paths_complete"], false);

    let record = report
        .conflict_record
        .clone()
        .expect("durable storm record");
    assert_eq!(record.paths.len(), path_count);
    let preserved = (
        record.preserved_base_ref.clone(),
        record.preserved_local_ref.clone(),
        record.preserved_remote_ref.clone(),
        record.preserved_record_ref.clone(),
    );
    let groups = conflict_groups(&record)
        .into_iter()
        .map(|group| group.id)
        .collect::<Vec<_>>();
    assert_eq!(groups.len(), path_count);
    assert_bounded_pages(&fixture, &fixture.store, &record.id, path_count);

    let first = &groups[..groups.len().min(128)];
    let before_dry_run = get_sync_conflict_with_state_store(
        &VaultPaths::new(&fixture.reader),
        &record.id,
        &fixture.store,
    )
    .expect("progress before dry-run");
    resolve_sync_conflict_with_state_store(
        &VaultPaths::new(&fixture.reader),
        &record.id,
        &resolution_options(first, true),
        &fixture.store,
    )
    .expect("dry-run batch");
    let after_dry_run = get_sync_conflict_with_state_store(
        &VaultPaths::new(&fixture.reader),
        &record.id,
        &fixture.store,
    )
    .expect("progress after dry-run");
    assert_eq!(after_dry_run.progress, before_dry_run.progress);
    resolve_group_batch(&fixture, &fixture.store, &record.id, first);

    let reopened = SyncStateStore::at(fixture.store.root().to_path_buf());
    let after_first = get_sync_conflict_with_state_store(
        &VaultPaths::new(&fixture.reader),
        &record.id,
        &reopened,
    )
    .expect("reopened conflict progress");
    assert_eq!(
        after_first.progress.pending_groups,
        path_count - first.len()
    );
    fs::write(
        fixture.reader.join("unrelated.md"),
        "unrelated advancement\n",
    )
    .expect("unrelated edit");
    sync_git_vault_with_state_store(
        &VaultPaths::new(&fixture.reader),
        &GitSyncOptions::default(),
        &reopened,
    )
    .expect("unrelated accepted advancement");

    let batches = if finish_all_groups {
        &groups[first.len()..]
    } else {
        &groups[first.len()..groups.len().min(first.len() + 128)]
    };
    for batch in batches.chunks(128) {
        resolve_group_batch(&fixture, &reopened, &record.id, batch);
    }
    let detail = get_sync_conflict_with_state_store(
        &VaultPaths::new(&fixture.reader),
        &record.id,
        &reopened,
    )
    .expect("final storm detail");
    let retained = detail.record;
    assert_eq!(
        (
            retained.preserved_base_ref.clone(),
            retained.preserved_local_ref.clone(),
            retained.preserved_remote_ref.clone(),
            retained.preserved_record_ref.clone(),
        ),
        preserved
    );
    assert_eq!(retained.paths, record.paths);
    if finish_all_groups {
        assert_eq!(detail.resolution, SyncConflictResolutionState::Resolved);
        assert_eq!(detail.progress.pending_groups, 0);
        assert_eq!(
            list_sync_conflicts_with_state_store(&VaultPaths::new(&fixture.reader), &reopened)
                .expect("final conflict list")
                .count,
            0
        );
        assert_final_storm_tree(&fixture, path_count);
        resolve_group_batch(
            &fixture,
            &reopened,
            &record.id,
            groups.last().map(std::slice::from_ref).expect("last group"),
        );
    } else {
        assert_eq!(detail.resolution, SyncConflictResolutionState::Unresolved);
        assert_eq!(detail.progress.pending_groups, path_count - 256);
    }
    assert_eq!(
        git_stdout(&fixture.reader, &["ls-files", "--stage", "-z"]),
        normal_index_before
    );
    eprintln!(
        "conflict storm: paths={path_count} report_bytes={} subprocesses={:?} elapsed_ms={} backend_ms={} conflict_state_ms={}",
        encoded.len(),
        report.operational_stats.git_subprocesses,
        report.operational_stats.elapsed_ms,
        report.operational_stats.backend_cycle_ms,
        report.operational_stats.conflict_state_ms,
    );
}

#[test]
fn conflict_storm_1000_lifecycle() {
    run_conflict_storm(1_000, true);
}

#[test]
#[ignore = "dedicated real-Git conflict scale acceptance"]
fn conflict_storm_10000_lifecycle() {
    run_conflict_storm(10_000, false);
}
