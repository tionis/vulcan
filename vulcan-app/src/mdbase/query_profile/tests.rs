use super::*;
use crate::mdbase::build_mdbase_query_report;
use crate::mdbase::tests::{fixture, read_control_grant};
use serde_json::json;
use vulcan_core::permissions::{PathPermission, ResourceSpecifier};

#[test]
fn query_sql_snapshot_matches_sources_preserves_links_and_rejects_inconsistent_columns() {
    let (_directory, paths) = fixture();
    let query = json!({"types": ["task"], "where": "title == 'Public'",
        "select": ["title", {"name": "target", "expr": "link('private/secret.md').asFile().path"}]});
    let expected = build_mdbase_query_report(&paths, &query, None).unwrap();
    assert_eq!(expected.meta.total_count, 1);
    let mut metrics = MdbaseQueryMetrics::default();
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    let actual = build_mdbase_query_report_profiled(&paths, &query, None, &mut metrics).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(metrics.execution_work.sql_rejected, 1);
    assert_eq!(metrics.execution_work.filter_input_checks, 2);
    assert_eq!(metrics.execution_work.filter_evaluations, 1);
    assert_eq!(metrics.cached_load.sql_selected_records, 1);
    assert_eq!(
        actual.results[0].values.as_ref().unwrap()["target"],
        "tasks/private/secret.md"
    );
    let database = vulcan_core::CacheDatabase::open(&paths).unwrap();
    for statement in [
        "UPDATE mdbase_record_cache SET effective_frontmatter_json='{}'",
        "DELETE FROM mdbase_record_types",
    ] {
        database.connection().execute(statement, []).unwrap();
        assert_eq!(
            build_mdbase_query_report_profiled(&paths, &query, None, &mut metrics).unwrap(),
            expected
        );
        assert_eq!(metrics.execution_work.sql_rejected, 0);
        assert_eq!(metrics.execution_work.filter_evaluations, 2);
        database.connection().execute("UPDATE mdbase_record_cache SET effective_frontmatter_json=json_extract(local_record_json, '$.effective_frontmatter')", []).unwrap();
    }
}

#[test]
fn query_sql_rejected_records_keep_input_errors_and_restricted_visibility() {
    let (directory, paths) = fixture();
    // The expression selection keeps this on the ordinary cached path.
    let query = json!({"types": ["task"], "where": "title == 'Public'",
        "select": ["title", {"name": "copy", "expr": "title"}]});
    let indexed_query =
        json!({"types": ["task"], "where": "title == 'Public'", "select": ["title"]});
    let mut metrics = MdbaseQueryMetrics::default();
    std::fs::write(
        directory.path().join("tasks/private/secret.md"),
        format!(
            "---\ntype: task\ntitle: Secret\n---\n{}",
            "x".repeat(1024 * 1024)
        ),
    )
    .unwrap();
    let expected = build_mdbase_query_report(&paths, &query, None).unwrap_err();
    assert!(expected.to_string().contains("input value size"));
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    let actual =
        build_mdbase_query_report_profiled(&paths, &query, None, &mut metrics).unwrap_err();
    assert_eq!(actual.to_string(), expected.to_string());
    assert_eq!(metrics.execution_work.sql_rejected, 1);
    assert_eq!(metrics.execution_work.filter_evaluations, 0);
    let mut allow = read_control_grant();
    allow.push(ResourceSpecifier::Note("mdbase.lock.yaml".into()));
    let filter = PermissionFilter::new(PathPermission {
        allow,
        deny: vec![ResourceSpecifier::Folder("tasks/private/**".into())],
    });
    let result =
        build_mdbase_query_report_profiled(&paths, &query, Some(&filter), &mut metrics).unwrap();
    assert_eq!(result.meta.total_count, 1);
    assert_eq!(metrics.execution_work.filter_input_checks, 1);
    assert_eq!(metrics.execution_work.sql_rejected, 0);
    assert_eq!(metrics.cache_hits, 1);
    assert_eq!(metrics.cached_load.sql_selection_attempts, 1);

    // The indexed path declines the over-limit record and the ordinary path
    // reports the same error; hidden from a restricted reader, it is served.
    let error =
        build_mdbase_query_report_profiled(&paths, &indexed_query, None, &mut metrics).unwrap_err();
    assert_eq!(error.to_string(), expected.to_string());
    assert_eq!(metrics.indexed_hits, 0);
    let result =
        build_mdbase_query_report_profiled(&paths, &indexed_query, Some(&filter), &mut metrics)
            .unwrap();
    assert_eq!(result.meta.total_count, 1);
    assert_eq!(metrics.indexed_hits, 1);
    assert_eq!(metrics.indexed.type_candidates, 1);
    assert_eq!(metrics.indexed.hydrated, 1);
}

#[test]
fn query_metrics_preserve_reports_and_distinguish_source_refresh_and_cached_loads() {
    let (_directory, paths) = fixture();
    // The expression selection keeps this on the ordinary cached path.
    let query = json!({"types": ["task"], "select": ["title", {"name": "copy", "expr": "title"}],
        "limit": 1});
    let mut metrics = MdbaseQueryMetrics::default();
    let expected = build_mdbase_query_report(&paths, &query, None).unwrap();
    let actual = build_mdbase_query_report_profiled(&paths, &query, None, &mut metrics).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(metrics.source_loads, 1);
    assert_eq!(metrics.completed_manifests, 2);
    assert_eq!(metrics.completed_manifest_records, 4);
    assert_eq!(metrics.prepared_visible_records, 2);
    assert_eq!(metrics.cache_hits, 0);
    assert_eq!(metrics.cached_load.decoded_records, 0);
    assert!(metrics.completed_manifest_bytes > 0);
    assert!(!paths.cache_db().exists());

    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    assert_eq!(
        build_mdbase_query_report_profiled(&paths, &query, None, &mut metrics).unwrap(),
        expected
    );
    assert_eq!(metrics.cache_refresh_attempts, 1);
    assert_eq!(metrics.cache_attempts, 2);
    assert_eq!(metrics.cache_hits, 1);
    assert_eq!(metrics.source_loads, 0);
    assert_eq!(metrics.cached_load.decoded_records, 2);
    assert_eq!(metrics.cached_load.overlay_records, 2);
    assert_eq!(metrics.cached_load.overlay_passes, 1);

    assert_eq!(
        build_mdbase_query_report_profiled(&paths, &query, None, &mut metrics).unwrap(),
        expected
    );
    assert_eq!(metrics.cache_refresh_attempts, 0);
    assert_eq!(metrics.cache_attempts, 1);
    assert_eq!(metrics.cache_hits, 1);
    assert_eq!(metrics.cached_load.decoded_records, 2);
    assert!(metrics.total_seconds >= metrics.record_preparation_seconds);
    assert!(metrics.record_preparation_seconds >= metrics.cached_load.total_seconds);
    assert!(metrics.cached_load.total_seconds >= metrics.cached_load.collection_overlay_seconds);

    // An eligible plan over the stat-proven cache decodes nothing but its page.
    let indexed = json!({"types": ["task"], "select": ["title"], "limit": 1});
    let source = build_mdbase_query_report_profiled(&paths, &indexed, None, &mut metrics).unwrap();
    assert_eq!(metrics.indexed_hits, 1);
    assert_eq!(metrics.cache_attempts, 0);
    assert_eq!(metrics.completed_manifests, 0);
    assert_eq!(metrics.indexed.visible_records, 2);
    assert_eq!(metrics.indexed.hydrated, 1);
    assert_eq!(source.results.len(), 1);
}

#[test]
fn query_metrics_filter_hidden_work_and_reset_before_denied_operations() {
    let (directory, paths) = fixture();
    vulcan_core::initialize_vulcan_dir(&paths).unwrap();
    // The expression selection keeps this on the ordinary cached path.
    let query = json!({"select": ["title", {"name": "copy", "expr": "title"}]});
    build_mdbase_query_report(&paths, &query, None).unwrap();
    std::fs::write(directory.path().join("tasks/private/secret.md"), [0xff]).unwrap();
    let mut allow = read_control_grant();
    allow.push(ResourceSpecifier::Note("mdbase.lock.yaml".into()));
    let filter = PermissionFilter::new(PathPermission {
        allow,
        deny: vec![ResourceSpecifier::Folder("tasks/private/**".into())],
    });
    let mut metrics = MdbaseQueryMetrics::default();
    let result =
        build_mdbase_query_report_profiled(&paths, &query, Some(&filter), &mut metrics).unwrap();
    assert_eq!(result.meta.total_count, 1);
    assert_eq!(metrics.completed_manifest_records, 2);
    assert_eq!(metrics.prepared_visible_records, 1);
    assert_eq!(metrics.cached_load.decoded_records, 1);
    assert_eq!(metrics.cached_load.overlay_records, 1);
    assert_eq!(metrics.cache_refresh_attempts, 0);
    let serialized = serde_json::to_string(&metrics).unwrap();
    assert!(!serialized.contains("secret"));
    assert!(!serialized.contains("public.md"));

    let denied = PermissionFilter::new(PathPermission {
        allow: vec![],
        deny: vec![],
    });
    let error = build_mdbase_query_report_profiled(&paths, &query, Some(&denied), &mut metrics)
        .unwrap_err();
    assert_eq!(error.code(), Some("permission_denied"));
    assert_eq!(metrics.prepared_visible_records, 0);
    assert_eq!(metrics.completed_manifest_records, 0);
    assert_eq!(metrics.cache_attempts, 0);
    assert_eq!(metrics.cached_load, MdbaseCachedLoadMetrics::default());
    assert!(metrics.total_seconds >= metrics.collection_seconds);

    let no_lockfile = PermissionFilter::new(PathPermission {
        allow: read_control_grant(),
        deny: vec![ResourceSpecifier::Folder("tasks/private/**".into())],
    });
    build_mdbase_query_report_profiled(&paths, &query, Some(&no_lockfile), &mut metrics).unwrap();
    assert_eq!(metrics.source_loads, 1);
    assert_eq!(metrics.completed_manifests, 0);
    assert_eq!(metrics.cache_attempts, 0);
    assert_eq!(metrics.prepared_visible_records, 1);
}

#[test]
fn query_metrics_keep_preflight_errors_ahead_of_record_work() {
    let (_directory, paths) = fixture();
    let query = json!({"where": "broken("});
    let mut metrics = MdbaseQueryMetrics::default();
    let error = build_mdbase_query_report_profiled(&paths, &query, None, &mut metrics).unwrap_err();
    assert_eq!(
        error.to_string(),
        build_mdbase_query_report(&paths, &query, None)
            .unwrap_err()
            .to_string()
    );
    assert_eq!(metrics.completed_manifests, 0);
    assert_eq!(metrics.source_loads, 0);
    assert_eq!(metrics.cache_attempts, 0);
    assert!(metrics.execution_seconds.abs() < f64::EPSILON);
}

#[test]
#[ignore = "public-fixture release stage diagnostic; run alone with VULCAN_MDB_PROFILE_FIXTURE"]
fn shared_query_stage_benchmark() {
    use vulcan_core::{resolve_permission_profile, PermissionGuard, ProfilePermissionGuard};
    let root = std::path::PathBuf::from(
        std::env::var("VULCAN_MDB_PROFILE_FIXTURE").expect("public generated fixture root"),
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let count = usize::try_from(manifest["records"].as_u64().unwrap()).unwrap();
    assert!(matches!(count, 10_000 | 100_000));
    assert_eq!(manifest["generator_version"], 1);
    assert_eq!(manifest["seed"], 42);
    let profile = std::env::var("VULCAN_MDB_PROFILE_SCOPE").ok();
    assert!(profile
        .as_deref()
        .is_none_or(|name| name == "benchmark_public"));
    let paths = VaultPaths::new(root.join("collection"));
    let cases = [
        ("task", "open"),
        ("task", "active"),
        ("task", "done"),
        ("contact", "1"),
        ("contact", "4"),
        ("contact", "7"),
        ("project", "open"),
        ("project", "active"),
        ("project", "done"),
    ];
    for (iteration, (kind, parameter)) in cases.iter().cycle().take(10).enumerate() {
        let query_name = format!("{kind}-{parameter}.json");
        let query: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("queries").join(&query_name)).unwrap())
                .unwrap();
        let start = Instant::now();
        let permission_start = Instant::now();
        let guard = ProfilePermissionGuard::new(
            &paths,
            resolve_permission_profile(&paths, profile.as_deref()).unwrap(),
        );
        let filter = guard.read_filter();
        let permission_seconds = permission_start.elapsed().as_secs_f64();
        let mut metrics = MdbaseQueryMetrics::default();
        let result = build_mdbase_query_report_profiled(
            &paths,
            &query,
            (!filter.path_permission().is_unrestricted()).then_some(&filter),
            &mut metrics,
        );
        let report =
            result.unwrap_or_else(|error| panic!("query failed: {error}; metrics={metrics:?}"));
        let serialization_start = Instant::now();
        let bytes = serde_json::to_vec(&report).unwrap();
        let serialization_seconds = serialization_start.elapsed().as_secs_f64();
        let request_seconds = start.elapsed().as_secs_f64();
        let expected_indices = expected_indices(count, kind, parameter, profile.is_some());
        let expected = expected_indices.len();
        let expected_paths = expected_indices
            .iter()
            .take(50)
            .map(|index| {
                let visibility = if index % 10 == 0 { "private" } else { "public" };
                format!(
                    "{visibility}/{kind}/{:02}/record-{index:06}.md",
                    index % 100
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(report.meta.total_count, expected);
        assert_eq!(report.results.len(), expected.min(50));
        assert_eq!(
            report
                .results
                .iter()
                .map(|row| row.file["path"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected_paths
        );
        assert!(report.results.iter().all(|row| row.body.is_none()));
        assert!(report.diagnostics.is_empty());
        assert!(bytes.len() <= 256 * 1024);
        println!(
            "{}",
            json!({"measurement": "shared_query_stage_diagnostic", "acceptance_gate_result": "not_evaluated",
            "iteration": iteration, "query": query_name, "permission_profile": profile,
            "records": count, "exact_total": expected, "rows": report.results.len(),
            "serialized_bytes": bytes.len(), "request_seconds": request_seconds,
            "permission_seconds": permission_seconds, "serialization_seconds": serialization_seconds,
            "metrics": metrics})
        );
    }
}

fn expected_indices(count: usize, kind: &str, parameter: &str, restricted: bool) -> Vec<usize> {
    (0..count)
        .filter(|index| {
            let actual_kind = ["task", "contact", "project"][index % 3];
            let status = if index % 5 == 0 {
                "open"
            } else {
                ["open", "active", "done"][(index / 3) % 3]
            };
            actual_kind == kind
                && !(restricted && index % 10 == 0)
                && if kind == "contact" {
                    *index == parameter.parse::<usize>().unwrap()
                } else {
                    status == parameter
                }
        })
        .collect()
}

#[test]
fn query_metrics_benchmark_oracle_covers_defaults_and_restrictions() {
    assert_eq!(expected_indices(10_000, "task", "open", false).len(), 1556);
    assert_eq!(expected_indices(10_000, "task", "open", true).len(), 1222);
    assert_eq!(expected_indices(10_000, "contact", "1", true), [1]);
    assert!(expected_indices(12, "contact", "4", false).contains(&4));
    assert!(expected_indices(120, "task", "open", true)
        .windows(2)
        .all(|pair| pair[0] < pair[1]));
}
