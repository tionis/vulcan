use super::*;
use tempfile::TempDir;

fn synthetic_cache(count: usize) -> (TempDir, VaultPaths, CacheDatabase) {
    let temp = TempDir::new().unwrap();
    let paths = VaultPaths::new(temp.path());
    crate::initialize_vulcan_dir(&paths).unwrap();
    let database = CacheDatabase::open(&paths).unwrap();
    let tx = database.connection().unchecked_transaction().unwrap();
    for index in 0..count {
        tx.execute(
            "INSERT INTO documents
             (id, path, filename, extension, content_hash, file_size, file_mtime,
              parser_version, indexed_at)
             VALUES (?1, ?2, ?2, 'md', X'00', 1, ?3, 1, '')",
            params![
                index.to_string(),
                format!("{index}.md"),
                current_unix_timestamp().unwrap()
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    (temp, paths, database)
}

fn newest(connection: &Connection) -> CheckpointRecord {
    let id: String = connection.query_row(
        "SELECT id FROM checkpoints WHERE generation IS NOT NULL ORDER BY generation DESC LIMIT 1",
        [], |row| row.get(0),
    ).unwrap();
    load_checkpoint_records(connection)
        .unwrap()
        .into_iter()
        .find(|row| row.id == id)
        .unwrap()
}

fn assert_current_snapshot(connection: &Connection) -> (String, Vec<DocumentState>) {
    let record = newest(connection);
    let full = build_snapshot_state(connection).unwrap();
    let expected = &full.records[0];
    assert_eq!(
        (
            record.note_count,
            record.orphan_notes,
            record.stale_notes,
            record.resolved_links
        ),
        (
            expected.note_count,
            expected.orphan_notes,
            expected.stale_notes,
            expected.resolved_links
        )
    );
    let actual = load_checkpoint_documents(connection, &record.id).unwrap();
    assert_eq!(actual, full.documents);
    (record.id, actual)
}

#[test]
fn shared_versions_survive_retention_manual_replacement_and_reopen() {
    let (_temp, paths, database) = synthetic_cache(32);
    let connection = database.connection();
    record_scan_checkpoint(connection).unwrap();
    let manual = create_checkpoint(&paths, "baseline").unwrap();
    let original = load_checkpoint_documents(connection, &manual.id).unwrap();
    let mut expected = HashMap::new();
    for index in 0..60 {
        connection
            .execute(
                "UPDATE documents SET content_hash = ?1 WHERE id = '0'",
                [index.to_string().as_bytes()],
            )
            .unwrap();
        // Deliberately omit IDs: correctness must include all logical state.
        record_scan_checkpoint_incremental(connection, &[]).unwrap();
        let (id, states) = assert_current_snapshot(connection);
        expected.insert(id, states);
    }
    let records = load_checkpoint_records(connection).unwrap();
    assert_eq!(
        records.iter().filter(|row| row.source == "scan").count(),
        MAX_AUTOMATIC_SCAN_CHECKPOINTS
    );
    for record in records.iter().filter(|row| row.source == "scan") {
        assert_eq!(
            &load_checkpoint_documents(connection, &record.id).unwrap(),
            &expected[&record.id]
        );
    }
    assert_eq!(
        load_checkpoint_documents(connection, &manual.id).unwrap(),
        original
    );
    let versions: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM checkpoint_document_versions",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(versions, 32 + 23);
    create_checkpoint(&paths, "baseline").unwrap();
    assert_eq!(load_checkpoint_records(connection).unwrap().len(), 25);
    drop(database);
    let reopened = CacheDatabase::open(&paths).unwrap();
    assert_current_snapshot(reopened.connection());
    for record in load_checkpoint_records(reopened.connection())
        .unwrap()
        .iter()
        .filter(|row| row.source == "scan")
    {
        assert_eq!(
            &load_checkpoint_documents(reopened.connection(), &record.id).unwrap(),
            &expected[&record.id]
        );
    }
}

#[test]
fn snapshots_capture_indirect_graph_properties_age_and_deletion() {
    let (_temp, _paths, database) = synthetic_cache(3);
    let c = database.connection();
    record_scan_checkpoint(c).unwrap();
    let (initial_id, initial) = assert_current_snapshot(c);
    c.execute_batch(
        "INSERT INTO links (id, source_document_id, raw_text, link_kind,
         resolved_target_id, origin_context, byte_offset)
         VALUES ('link', '0', '[[1]]', 'wikilink', '1', 'body', 0);
         INSERT INTO properties(document_id, raw_yaml, canonical_json) VALUES ('2', 'changed: true', '{\"changed\":true}');
         UPDATE documents SET file_mtime = 1 WHERE id = '2';",
    )
    .unwrap();
    record_scan_checkpoint_incremental(c, &["0".into()]).unwrap();
    let (_, linked) = assert_current_snapshot(c);
    assert!(!linked[1].orphan);
    assert!(linked[2].stale);
    assert_ne!(linked[2].property_hash, initial[2].property_hash);
    // Re-resolving an unedited source changes its hash without its own scan.
    c.execute("UPDATE links SET resolved_target_id = NULL", [])
        .unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    let (_, unresolved) = assert_current_snapshot(c);
    assert_ne!(unresolved[0].link_hash, linked[0].link_hash);
    c.execute("UPDATE links SET resolved_target_id = '1'", [])
        .unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    assert_eq!(assert_current_snapshot(c).1, linked);
    c.execute(
        "UPDATE documents SET path = 'renamed.md' WHERE id = '1'",
        [],
    )
    .unwrap();
    record_scan_checkpoint_incremental(c, &["1".into()]).unwrap();
    let (_, renamed) = assert_current_snapshot(c);
    assert_ne!(renamed[0].link_hash, linked[0].link_hash);
    c.execute("DELETE FROM documents WHERE id = '1'", [])
        .unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    let (_, deleted) = assert_current_snapshot(c);
    assert!(deleted[0].orphan);
    assert_eq!(deleted.len(), 2);
    assert_eq!(load_checkpoint_documents(c, &initial_id).unwrap(), initial);
    // Reintroducing a deleted path opens a new interval.
    c.execute(
        "UPDATE documents SET path = 'renamed.md' WHERE id = '2'",
        [],
    )
    .unwrap();
    record_scan_checkpoint(c).unwrap();
    assert_current_snapshot(c);
    c.execute("DELETE FROM documents", []).unwrap();
    record_scan_checkpoint(c).unwrap();
    assert!(assert_current_snapshot(c).1.is_empty());
}

#[test]
fn checkpoint_failure_rolls_back_versions_header_and_pruning() {
    let (_temp, _paths, database) = synthetic_cache(8);
    let c = database.connection();
    for _ in 0..24 {
        record_scan_checkpoint(c).unwrap();
    }
    let before = load_checkpoint_records(c).unwrap();
    let snapshots: Vec<_> = before
        .iter()
        .map(|row| load_checkpoint_documents(c, &row.id).unwrap())
        .collect();
    c.execute_batch(
        "UPDATE documents SET content_hash = X'01' WHERE id = '0';
         CREATE TEMP TRIGGER fail_checkpoint_pruning BEFORE DELETE ON checkpoints
         BEGIN SELECT RAISE(ABORT, 'injected checkpoint prune failure'); END;",
    )
    .unwrap();
    assert!(record_scan_checkpoint(c).is_err());
    assert_eq!(load_checkpoint_records(c).unwrap(), before);
    for (record, expected) in before.iter().zip(snapshots) {
        assert_eq!(load_checkpoint_documents(c, &record.id).unwrap(), expected);
    }
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM checkpoint_document_versions WHERE valid_to IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    c.execute_batch("DROP TRIGGER fail_checkpoint_pruning")
        .unwrap();
    record_scan_checkpoint(c).unwrap();
    assert_current_snapshot(c);
}

#[test]
fn vector_changes_and_legacy_anchors_match_shared_diff_output() {
    let (_temp, paths, database) = synthetic_cache(2);
    let c = database.connection();
    c.execute_batch(
        "CREATE TABLE vectors (chunk_id TEXT PRIMARY KEY);
         INSERT INTO chunks (id, document_id, sequence_index, heading_path,
             byte_offset_start, byte_offset_end, content_hash, chunk_strategy, chunk_version, content)
         VALUES ('chunk', '0', 0, '', 0, 1, X'00', 'heading', 1, 'text');
         INSERT INTO vectors VALUES ('chunk');
         INSERT INTO vector_index_state (id, provider_name, model_name, dimensions, normalized)
         VALUES (1, 'test', 'model', 2, 1);",
    ).unwrap();
    record_scan_checkpoint(c).unwrap();
    let baseline = create_checkpoint(&paths, "legacy").unwrap();
    // Simulate an old automatic header: legacy rows remain self-contained.
    c.execute(
        "UPDATE checkpoints SET source = 'scan' WHERE id = ?1",
        [&baseline.id],
    )
    .unwrap();
    c.execute_batch("UPDATE vector_index_state SET model_name = 'new-model'; UPDATE documents SET content_hash = X'01' WHERE id = '1';").unwrap();
    let automatic = query_change_report(&paths, &ChangeAnchor::LastScan).unwrap();
    let named = query_change_report(&paths, &ChangeAnchor::Checkpoint("legacy".into())).unwrap();
    assert_eq!(automatic.notes, named.notes);
    assert_eq!(automatic.embeddings, named.embeddings);
    assert_eq!(automatic.embeddings.len(), 1);
    record_scan_checkpoint_incremental(c, &["1".into()]).unwrap();
    assert_current_snapshot(c);
    // LastScan intentionally selects the penultimate automatic checkpoint.
    assert_eq!(
        query_change_report(&paths, &ChangeAnchor::LastScan)
            .unwrap()
            .embeddings
            .len(),
        1
    );
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    assert!(query_change_report(&paths, &ChangeAnchor::LastScan)
        .unwrap()
        .embeddings
        .is_empty());
    c.execute("DELETE FROM vectors", []).unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    assert!(assert_current_snapshot(c).1[0].embedding_hash.is_empty());
}

#[test]
fn checkpoint_order_remains_monotonic_after_clock_regression() {
    let (_temp, _paths, database) = synthetic_cache(1);
    let c = database.connection();
    record_scan_checkpoint(c).unwrap();
    let future = current_unix_timestamp().unwrap() + 3_600;
    c.execute("UPDATE checkpoints SET created_at = ?1", [future])
        .unwrap();
    for _ in 0..30 {
        record_scan_checkpoint(c).unwrap();
    }
    let records = load_checkpoint_records(c).unwrap();
    assert_eq!(records[0].id, newest(c).id);
    assert_eq!(records[0].created_at, future);
    assert_current_snapshot(c);
}

#[test]
fn manual_snapshots_leave_automatic_dirty_state_intact_across_reopen_and_rebuild() {
    let (_temp, paths, database) = synthetic_cache(2);
    let c = database.connection();
    c.execute_batch(
        "INSERT INTO properties(document_id, raw_yaml, canonical_json) VALUES ('0', 'value: 1', '{\"value\":1}');
         CREATE TABLE vectors (chunk_id TEXT PRIMARY KEY);
         INSERT INTO chunks (id, document_id, sequence_index, heading_path,
             byte_offset_start, byte_offset_end, content_hash, chunk_strategy, chunk_version, content)
         VALUES ('chunk', '1', 0, '', 0, 1, X'00', 'heading', 1, 'text');
         INSERT INTO vector_index_state (id, provider_name, model_name, dimensions, normalized)
         VALUES (1, 'test', 'model', 2, 1);",
    ).unwrap();
    record_scan_checkpoint(c).unwrap();
    // Keep two automatic baselines for the penultimate-scan diff contract.
    record_scan_checkpoint(c).unwrap();
    let (old_id, old_states) = assert_current_snapshot(c);
    c.execute_batch("UPDATE properties SET raw_yaml = 'value: 2', canonical_json = '{\"value\":2}'; INSERT INTO vectors VALUES ('chunk');").unwrap();
    let manual = create_checkpoint(&paths, "midway").unwrap();
    let expected = load_checkpoint_documents(c, &manual.id).unwrap();
    assert_eq!(
        c.query_row(
            "SELECT COUNT(*) FROM checkpoint_dirty_documents",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM checkpoint_vector_inputs", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
    drop(database);
    let mut reopened = CacheDatabase::open(&paths).unwrap();
    let before = query_change_report(&paths, &ChangeAnchor::LastScan).unwrap();
    assert_eq!(before.properties.len(), 1);
    assert_eq!(before.embeddings.len(), 1);
    record_scan_checkpoint_incremental(reopened.connection(), &[]).unwrap();
    assert_eq!(assert_current_snapshot(reopened.connection()).1, expected);
    assert_eq!(
        load_checkpoint_documents(reopened.connection(), &old_id).unwrap(),
        old_states
    );
    reopened.clear_all().unwrap();
    let c = reopened.connection();
    for table in ["checkpoint_dirty_documents", "checkpoint_vector_inputs"] {
        assert_eq!(
            c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    // An incremental retry after a failed rebuild checkpoint must not reuse
    // hashes whose invalidation inputs were cleared by the rebuild.
    assert_eq!(
        c.query_row(
            "SELECT value FROM meta WHERE key = 'checkpoint_reset'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "1"
    );
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    assert!(assert_current_snapshot(c).1.is_empty());
    assert_eq!(load_checkpoint_documents(c, &manual.id).unwrap(), expected);
    assert_eq!(load_checkpoint_documents(c, &old_id).unwrap(), old_states);
}

fn wal_bytes(paths: &VaultPaths) -> u64 {
    std::fs::metadata(format!("{}-wal", paths.cache_db().display()))
        .unwrap()
        .len()
}

#[test]
fn single_edit_row_writes_and_wal_measurements() {
    for count in [100, 1_000, 10_000] {
        let (_temp, paths, database) = synthetic_cache(count);
        let c = database.connection();
        c.execute_batch("PRAGMA wal_autocheckpoint = 0").unwrap();
        c.execute(
            "INSERT INTO properties(document_id, raw_yaml, canonical_json)
             SELECT id, 'value: 1', '{\"value\":1}' FROM documents",
            [],
        )
        .unwrap();
        record_scan_checkpoint(c).unwrap();
        c.execute(
            "UPDATE documents SET content_hash = X'01' WHERE id = '0'",
            [],
        )
        .unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        let before = c.total_changes();
        CHECKPOINT_HASH_CALLS.with(|calls| calls.set(0));
        let start = std::time::Instant::now();
        record_scan_checkpoint_incremental(c, &["0".into()]).unwrap();
        assert_eq!(
            CHECKPOINT_HASH_CALLS.with(std::cell::Cell::get),
            1,
            "only the edited property projection should be hashed"
        );
        let elapsed = start.elapsed();
        let shared_writes = c.total_changes() - before;
        let shared_wal = wal_bytes(&paths);
        assert_eq!(
            shared_writes, 6,
            "header insert/update, one close, one insert, the clock, the consumed path mark"
        );
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        // The preserved full-snapshot writer is the copying baseline. The old
        // incremental path additionally updated its header once.
        let before = c.total_changes();
        CHECKPOINT_HASH_CALLS.with(|calls| calls.set(0));
        let start = std::time::Instant::now();
        let tx = c.unchecked_transaction().unwrap();
        insert_checkpoint_snapshot(&tx, Some("copy-baseline"), "manual").unwrap();
        tx.commit().unwrap();
        assert_eq!(CHECKPOINT_HASH_CALLS.with(std::cell::Cell::get), count);
        let baseline_elapsed = start.elapsed();
        let copied_writes = c.total_changes() - before;
        let copied_wal = wal_bytes(&paths);
        assert_eq!(copied_writes, u64::try_from(count).unwrap() + 1);
        eprintln!("checkpoint measurement: notes={count}, shared_rows={shared_writes}, copy_rows={copied_writes}, shared_wal={shared_wal}, copy_wal={copied_wal}, shared_time={elapsed:?}, copy_time={baseline_elapsed:?}");
        for _ in 2..MAX_AUTOMATIC_SCAN_CHECKPOINTS {
            record_scan_checkpoint(c).unwrap();
        }
        c.execute(
            "UPDATE documents SET content_hash = X'02' WHERE id = '0'",
            [],
        )
        .unwrap();
        c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
        let before = c.total_changes();
        record_scan_checkpoint_incremental(c, &["0".into()]).unwrap();
        let retained_writes = c.total_changes() - before;
        assert_eq!(retained_writes, 8, "also delete expired header and version");
        eprintln!(
            "checkpoint retention: notes={count}, rows={retained_writes}, wal={}",
            wal_bytes(&paths)
        );
    }
}

/// Incremental checkpoints need no IDs to see a target orphaned by its
/// sources' link edits, or a document that aged past the staleness
/// threshold since the previous checkpoint without being edited.
#[test]
fn incremental_checkpoints_track_orphaned_targets_and_aging_without_ids() {
    let (_temp, _paths, database) = synthetic_cache(4);
    let c = database.connection();
    record_scan_checkpoint(c).unwrap();
    c.execute_batch(
        "INSERT INTO links (id, source_document_id, raw_text, link_kind,
         resolved_target_id, origin_context, byte_offset)
         VALUES ('a', '0', '[[1]]', 'wikilink', '1', 'body', 0),
                ('b', '2', '[[1]]', 'wikilink', '1', 'body', 0);",
    )
    .unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    let (_, linked) = assert_current_snapshot(c);
    assert!(!linked[1].orphan && linked[3].orphan);
    c.execute("DELETE FROM links WHERE id = 'a'", []).unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    assert!(assert_current_snapshot(c).1[0].orphan);
    // The target's last inbound link goes; only the trigger names it.
    c.execute("DELETE FROM links WHERE id = 'b'", []).unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    assert!(assert_current_snapshot(c).1[1].orphan);

    // Aging: present the previous checkpoint as evaluated just before
    // document 3 crossed the threshold, as if time had passed since.
    let now = current_unix_timestamp().unwrap();
    let mtime = now - STALE_AGE_SECS + 1_000;
    c.execute(
        "UPDATE documents SET file_mtime = ?1 WHERE id = '3'",
        [mtime],
    )
    .unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    assert!(!assert_current_snapshot(c).1[3].stale);
    c.execute_batch(&format!(
        "UPDATE documents SET file_mtime = {old} WHERE id = '3';
         UPDATE checkpoint_document_versions SET stale = 0
         WHERE path = '3.md' AND valid_to IS NULL;
         DELETE FROM checkpoint_path_dirty;
         UPDATE meta SET value = '{clock}' WHERE key = 'checkpoint_clock';",
        old = now - STALE_AGE_SECS - 10,
        clock = now - 1_000,
    ))
    .unwrap();
    record_scan_checkpoint_incremental(c, &[]).unwrap();
    assert!(assert_current_snapshot(c).1[3].stale);
}
