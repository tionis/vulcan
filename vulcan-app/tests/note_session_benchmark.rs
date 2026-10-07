//! Developer-only note-query service diagnostic (QRY.6/QRY.7): the vault HTTP
//! route handlers in process, with or without a retained note-store session,
//! under closed-loop readers and an optional paced writer. Run alone against
//! a generated public fixture (`scripts/generate_mdb_fixture.py`) that has
//! been scanned with the current build:
//!
//! ```sh
//! VULCAN_NOTE_BENCH_FIXTURE=/tmp/mdb-10k/collection \
//!   cargo +1.88.0 test --release -p vulcan-app --test note_session_benchmark \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Variables: `VULCAN_NOTE_BENCH_SAMPLES` (per reader, default 100),
//! `VULCAN_NOTE_BENCH_READERS` (default 8), `VULCAN_NOTE_BENCH_WRITES_PER_SECOND`
//! (default 0), `VULCAN_NOTE_BENCH_SESSION` (`0` for the direct path),
//! `VULCAN_NOTE_BENCH_SCOPE` (a permission profile), and
//! `VULCAN_NOTE_BENCH_FRONTENDS` (a comma-separated subset of `dql`,
//! `query`, `bases`, `notes`, and `mdbase`). The writer edits a record body
//! outside mdbase's managed write path, so mixed runs that include mdbase
//! measure its disk-reconciled fallback. The benchmark adds
//! `public/_bench/*.base` files (readable by the fixture's `benchmark_public`
//! profile) for the run and removes them afterwards. Its JSON
//! report labels the acceptance gate `not_evaluated`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use vulcan_app::serve::{
    route_request_with_sessions, ServeHealthState, ServeRequest, ServeRouteOptions, ServeSessions,
};
use vulcan_core::note_session::NoteStoreSession;
use vulcan_core::VaultPaths;

const TYPES: [&str; 3] = ["task", "project", "contact"];
const STATUSES: [&str; 2] = ["active", "done"];
const WRITTEN: &str = "public/contact/01/record-000001.md";

fn env_number(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn get(path: &str, params: &[(&str, String)]) -> ServeRequest {
    ServeRequest {
        method: "GET".to_string(),
        path: path.to_string(),
        query: params
            .iter()
            .map(|(key, value)| ((*key).to_string(), vec![value.clone()]))
            .collect(),
    }
}

/// One parameter-varied request per frontend, type, and status.
fn requests() -> Vec<(&'static str, ServeRequest)> {
    let mut requests = Vec::new();
    for kind in TYPES {
        for status in STATUSES {
            requests.push((
                "dql",
                get(
                    "/dataview/query",
                    &[(
                        "dql",
                        format!(
                            "TABLE title, status, priority FROM \"public/{kind}\" \
                             WHERE status = \"{status}\" SORT title ASC LIMIT 50"
                        ),
                    )],
                ),
            ));
            requests.push((
                "query",
                get(
                    "/query",
                    &[(
                        "dsl",
                        format!(
                            "from notes where type = {kind} and status = {status} \
                             order by title limit 50"
                        ),
                    )],
                ),
            ));
            requests.push((
                "bases",
                get(
                    "/bases/eval",
                    &[("file", format!("public/_bench/{kind}-{status}.base"))],
                ),
            ));
            requests.push((
                "mdbase",
                get(
                    "/mdbase/query",
                    &[(
                        "query",
                        serde_json::json!({
                            "types": [kind],
                            "where": format!("status == \"{status}\""),
                            "order_by": [{"field": "title", "direction": "asc"}],
                            "select": ["file.path", "title", "status", "priority"],
                            "limit": 50,
                        })
                        .to_string(),
                    )],
                ),
            ));
            requests.push((
                "notes",
                get(
                    "/notes",
                    &[
                        ("where", format!("type = {kind}")),
                        ("limit", "50".to_string()),
                    ],
                ),
            ));
        }
    }
    requests
}

/// The paths a response names, in order: the checked part of an answer.
fn answer_paths(body: &serde_json::Value) -> Vec<String> {
    let result = &body["result"];
    let rows = result["rows"]
        .as_array()
        .or_else(|| result["notes"].as_array())
        .cloned()
        .or_else(|| result["views"][0]["rows"].as_array().cloned())
        .or_else(|| result["results"].as_array().cloned())
        .unwrap_or_default();
    rows.iter()
        .map(|row| {
            row["document_path"]
                .as_str()
                .or_else(|| row["file"]["path"].as_str())
                .map_or_else(|| row.to_string(), ToString::to_string)
        })
        .collect()
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let rank = ((sorted.len() as f64) * fraction).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn summary(mut samples: Vec<f64>) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    serde_json::json!({
        "count": samples.len(),
        "p50_ms": percentile(&samples, 0.50),
        "p95_ms": percentile(&samples, 0.95),
        "p99_ms": percentile(&samples, 0.99),
        "max_ms": samples.last().copied().unwrap_or_default(),
    })
}

fn load_average() -> Option<String> {
    fs::read_to_string("/proc/loadavg").ok().map(|line| {
        line.split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// A body-only edit of one record in one write section, with the note scan
/// a daemon watcher would run, as a managed write followed by its refresh.
fn write_once(paths: &VaultPaths, original: &str, round: usize) -> f64 {
    let start = Instant::now();
    let lock = vulcan_core::write_lock::acquire_write_lock(paths).expect("write lock");
    let content = if round % 2 == 0 {
        format!("{original}\nBenchmark edit {round}.\n")
    } else {
        original.to_string()
    };
    fs::write(paths.vault_root().join(WRITTEN), content).expect("write record");
    vulcan_core::scan::scan_vault_paths_unlocked(paths, &BTreeSet::from([WRITTEN.to_string()]))
        .expect("scan written path");
    drop(lock);
    start.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore = "public-fixture note-query diagnostic; run alone with VULCAN_NOTE_BENCH_FIXTURE"]
#[allow(clippy::too_many_lines)]
fn note_query_service_benchmark() {
    let root = std::env::var("VULCAN_NOTE_BENCH_FIXTURE").expect("generated fixture collection");
    let paths = VaultPaths::new(&root);
    let samples = env_number("VULCAN_NOTE_BENCH_SAMPLES", 100);
    let readers = env_number("VULCAN_NOTE_BENCH_READERS", 8).max(1);
    let writes_per_second = env_number("VULCAN_NOTE_BENCH_WRITES_PER_SECOND", 0);
    let use_session = std::env::var("VULCAN_NOTE_BENCH_SESSION").map_or(true, |value| value != "0");
    let options = ServeRouteOptions {
        permissions: std::env::var("VULCAN_NOTE_BENCH_SCOPE")
            .ok()
            .filter(|scope| !scope.is_empty()),
        watch_enabled: false,
    };

    let bases = paths.vault_root().join("public/_bench");
    fs::create_dir_all(&bases).expect("bench directory");
    for kind in TYPES {
        for status in STATUSES {
            fs::write(
                bases.join(format!("{kind}-{status}.base")),
                format!(
                    "filters:\n  and:\n    - 'type == \"{kind}\"'\n    - 'status == \"{status}\"'\n\
                     views:\n  - type: table\n    name: rows\n    order:\n      - title\n      - status\n      - priority\n    sort:\n      - property: title\n        direction: ASC\n    limit: 50\n"
                ),
            )
            .expect("base file");
        }
    }
    vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Incremental).expect("scan bases");
    let original = fs::read_to_string(paths.vault_root().join(WRITTEN)).expect("written record");

    let state = ServeHealthState::default();
    let session = NoteStoreSession::new(paths.clone());
    // The daemon's watched mdbase session.
    let mdbase = vulcan_app::mdbase::MdbaseQuerySession::new(paths.clone()).with_change_monitor(
        vulcan_core::mdbase::MdbaseChangeMonitor::watch(paths.vault_root()).expect("monitor"),
        Duration::from_secs(30),
    );
    let sessions = ServeSessions {
        mdbase: use_session.then_some(&mdbase),
        notes: use_session.then_some(&session),
    };
    // `VULCAN_NOTE_BENCH_FRONTENDS` selects frontends (comma-separated).
    let frontends = std::env::var("VULCAN_NOTE_BENCH_FRONTENDS").ok();
    let requests = requests()
        .into_iter()
        .filter(|(frontend, _)| {
            frontends
                .as_deref()
                .is_none_or(|selected| selected.split(',').any(|name| name == *frontend))
        })
        .collect::<Vec<_>>();
    // Expected answers come from the direct path before any reader runs.
    let expected = requests
        .iter()
        .map(|(_, request)| {
            let response = vulcan_app::serve::route_request(&paths, &options, &state, request);
            assert_eq!(response.status, 200, "{}: {}", request.path, response.body);
            let answer = answer_paths(&response.body);
            assert!(!answer.is_empty(), "{:?} matched nothing", request.query);
            answer
        })
        .collect::<Vec<_>>();

    let latencies = Mutex::new(BTreeMap::<&str, Vec<f64>>::new());
    let write_latencies = Mutex::new(Vec::new());
    let stop = AtomicBool::new(false);
    let started = Instant::now();
    std::thread::scope(|scope| {
        let writer = (writes_per_second > 0).then(|| {
            scope.spawn(|| {
                let interval = Duration::from_secs_f64(
                    1.0 / f64::from(u32::try_from(writes_per_second).unwrap_or(1)),
                );
                let mut round = 0;
                let mut next = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let elapsed = write_once(&paths, &original, round);
                    write_latencies.lock().unwrap().push(elapsed);
                    round += 1;
                    next += interval;
                    if let Some(wait) = next.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                }
                // Leave the record as generated.
                if round % 2 == 1 {
                    write_once(&paths, &original, round);
                }
            })
        });
        let handles = (0..readers)
            .map(|reader| {
                let requests = &requests;
                let expected = &expected;
                let latencies = &latencies;
                let state = &state;
                let options = &options;
                let paths = &paths;
                scope.spawn(move || {
                    let mut local = HashMap::<&str, Vec<f64>>::new();
                    for sample in 0..=samples {
                        let index = (reader * 7 + sample) % requests.len();
                        let (frontend, request) = &requests[index];
                        let start = Instant::now();
                        let response =
                            route_request_with_sessions(paths, options, state, request, sessions);
                        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                        assert_eq!(response.status, 200, "{}", response.body);
                        assert_eq!(
                            answer_paths(&response.body),
                            expected[index],
                            "{:?}",
                            request.query
                        );
                        // Each reader's first request is excluded.
                        if sample > 0 {
                            local.entry(frontend).or_default().push(elapsed);
                        }
                    }
                    let mut latencies = latencies.lock().unwrap();
                    for (frontend, samples) in local {
                        latencies.entry(frontend).or_default().extend(samples);
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("reader");
        }
        stop.store(true, Ordering::Relaxed);
        if let Some(writer) = writer {
            writer.join().expect("writer");
        }
    });
    let wall = started.elapsed().as_secs_f64();
    fs::remove_dir_all(&bases).expect("remove bench bases");
    vulcan_core::scan_vault(&paths, vulcan_core::ScanMode::Incremental).expect("scan removal");

    let latencies = latencies.into_inner().unwrap();
    let all = latencies.values().flatten().copied().collect::<Vec<_>>();
    let counters = session.counters();
    let counter = |value: &std::sync::atomic::AtomicU64| value.load(Ordering::Relaxed);
    let report = serde_json::json!({
        "measurement": "note_query_service_diagnostic",
        "acceptance_gate_result": "not_evaluated",
        "boundary": "in-process vault HTTP route handlers: per-request permission-profile resolution, authorization, execution, and JSON response construction; excludes HTTP transport",
        "session": use_session,
        "scope": options.permissions,
        "frontends": frontends,
        "readers": readers,
        "samples_per_reader": samples,
        "writes_per_second_target": writes_per_second,
        "wall_seconds": wall,
        "load_average_after": load_average(),
        "requests": all.len(),
        "all": summary(all),
        "by_frontend": latencies
            .into_iter()
            .map(|(frontend, samples)| (frontend, summary(samples)))
            .collect::<BTreeMap<_, _>>(),
        "writes": summary(write_latencies.into_inner().unwrap()),
        "session_counters": {
            "snapshots": counter(&counters.snapshots),
            "snapshots_unavailable": counter(&counters.snapshots_unavailable),
            "connections_opened": counter(&counters.connections_opened),
            "identity_loads": counter(&counters.identity_loads),
            "identity_refreshes": counter(&counters.identity_refreshes),
            "identity_reuses": counter(&counters.identity_reuses),
            "stored_loaded": counter(&counters.stored_loaded),
            "stored_reused": counter(&counters.stored_reused),
            "hydrated_loaded": counter(&counters.hydrated_loaded),
            "hydrated_reused": counter(&counters.hydrated_reused),
            "hydrated_carried": counter(&counters.hydrated_carried),
        },
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
