//! The stdio MCP transport: newline-delimited JSON-RPC, one message per line.
//!
//! A reader takes each line as it arrives. Tool calls go to a bounded pool of
//! workers, so a slow call does not hold up reads behind it, and
//! `notifications/cancelled` can still reach a call that waits or runs.
//! Everything else is answered in order on the reader. One writer serializes
//! what goes to stdout, and a change watcher tells a legacy client when its
//! tool, prompt, or resource lists change, without being asked.

use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::McpServerCore;
use vulcan_app::mcp_dispatch::jsonrpc_error;

/// Tool calls that may run at once.
const WORKERS: usize = 4;
/// Tool calls that may wait for a worker before the reader stops reading.
const QUEUE: usize = 64;
/// How often the change watcher looks for list changes.
const WATCH_INTERVAL: Duration = Duration::from_secs(2);
/// How long calls still running at end of input may take to finish.
const DRAIN: Duration = Duration::from_secs(5);

type Output = Arc<Mutex<Box<dyn Write + Send>>>;

fn send(output: &Output, message: &Value) {
    let mut output = output.lock().unwrap_or_else(PoisonError::into_inner);
    // A closed stdout ends the session at the next read; nothing to report to.
    let _ = writeln!(
        output,
        "{}",
        serde_json::to_string(message).unwrap_or_default()
    );
    let _ = output.flush();
}

/// Tool calls waiting or running, and which of them the client cancelled.
#[derive(Default)]
struct Calls {
    pending: HashSet<String>,
    cancelled: HashSet<String>,
}

struct Tracker {
    calls: Mutex<Calls>,
    idle: Condvar,
}

impl Tracker {
    fn lock(&self) -> std::sync::MutexGuard<'_, Calls> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cancel(&self, key: String) {
        let mut calls = self.lock();
        // Only a call still pending can be cancelled; others are done.
        if calls.pending.contains(&key) {
            calls.cancelled.insert(key);
        }
    }

    fn is_cancelled(&self, key: &str) -> bool {
        self.lock().cancelled.contains(key)
    }

    fn finish(&self, key: &str) {
        let mut calls = self.lock();
        calls.pending.remove(key);
        calls.cancelled.remove(key);
        if calls.pending.is_empty() {
            self.idle.notify_all();
        }
    }

    /// Wait until no call is pending, or `limit` passes.
    fn drain(&self, limit: Duration) {
        let deadline = Instant::now() + limit;
        let mut calls = self.lock();
        while !calls.pending.is_empty() {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                return;
            };
            calls = self
                .idle
                .wait_timeout(calls, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

fn id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

/// Serve MCP over `input` and `output` until `input` ends.
pub(super) fn serve(
    mut core: McpServerCore,
    request_timeout: Duration,
    input: impl BufRead,
    output: Box<dyn Write + Send>,
) -> std::io::Result<()> {
    core.watch_list_changes();
    let core = Arc::new(Mutex::new(core));
    let output: Output = Arc::new(Mutex::new(output));
    let tracker = Arc::new(Tracker {
        calls: Mutex::new(Calls::default()),
        idle: Condvar::new(),
    });

    let (queue, jobs) = mpsc::sync_channel::<(String, Value)>(QUEUE);
    let jobs = Arc::new(Mutex::new(jobs));
    let workers = (0..WORKERS)
        .map(|index| {
            let (core, output, tracker, jobs) = (
                Arc::clone(&core),
                Arc::clone(&output),
                Arc::clone(&tracker),
                Arc::clone(&jobs),
            );
            std::thread::Builder::new()
                .name(format!("vulcan-mcp-stdio-{index}"))
                .spawn(move || loop {
                    let job = jobs.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok((key, request)) = job else { return };
                    if !tracker.is_cancelled(&key) {
                        // The core keeps no per-call state: each call runs on
                        // a copy, and the shared core stays free for the reader.
                        let mut worker =
                            core.lock().unwrap_or_else(PoisonError::into_inner).clone();
                        let messages =
                            worker.process_request_with_timeout(request, request_timeout);
                        // A cancelled call gets no response.
                        if !tracker.is_cancelled(&key) {
                            for message in &messages {
                                send(&output, message);
                            }
                        }
                    }
                    tracker.finish(&key);
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (core, output, stop) = (Arc::clone(&core), Arc::clone(&output), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("vulcan-mcp-stdio-watch".to_string())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::park_timeout(WATCH_INTERVAL);
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let changes = core
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .poll_list_changes();
                    for message in &changes {
                        send(&output, message);
                    }
                }
            })?
    };

    let result = read_requests(input, &core, &output, &tracker, &queue, request_timeout);

    drop(queue);
    tracker.drain(DRAIN);
    stop.store(true, Ordering::Relaxed);
    watcher.thread().unpark();
    let _ = watcher.join();
    // Workers still busy after the drain are abandoned with the process.
    if tracker.lock().pending.is_empty() {
        for worker in workers {
            let _ = worker.join();
        }
    }
    result
}

fn read_requests(
    input: impl BufRead,
    core: &Arc<Mutex<McpServerCore>>,
    output: &Output,
    tracker: &Tracker,
    queue: &mpsc::SyncSender<(String, Value)>,
    request_timeout: Duration,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => value,
            Err(error) => {
                send(
                    output,
                    &jsonrpc_error(Value::Null, -32700, format!("Parse error: {error}"), None),
                );
                continue;
            }
        };
        let method = request.get("method").and_then(Value::as_str);
        if method == Some("notifications/cancelled") {
            if let Some(id) = request
                .get("params")
                .and_then(|params| params.get("requestId"))
            {
                tracker.cancel(id_key(id));
            }
            continue;
        }
        if let (Some("tools/call"), Some(id)) = (method, request.get("id")) {
            let key = id_key(id);
            tracker.lock().pending.insert(key.clone());
            if queue.send((key.clone(), request)).is_err() {
                tracker.finish(&key);
            }
            continue;
        }
        // Only `initialize` changes the connection; everything else runs on
        // a copy, so the workers and the watcher are not held up meanwhile.
        let messages = if method == Some("initialize") {
            core.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .process_request_with_timeout(request, request_timeout)
        } else {
            let mut copy = core.lock().unwrap_or_else(PoisonError::into_inner).clone();
            copy.process_request_with_timeout(request, request_timeout)
        };
        for message in &messages {
            send(output, message);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;
    use vulcan_core::VaultPaths;

    /// An output that tests can read after the server is done with it.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Captured {
        fn messages(&self) -> Vec<Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

    fn run(root: &std::path::Path, lines: &[Value]) -> Vec<Value> {
        let paths = VaultPaths::new(root);
        let core = McpServerCore::new(&paths, Some("readonly")).unwrap();
        let input = lines
            .iter()
            .map(|line| serde_json::to_string(line).unwrap() + "\n")
            .collect::<String>();
        let captured = Captured::default();
        serve(
            core,
            Duration::from_secs(30),
            Cursor::new(input),
            Box::new(captured.clone()),
        )
        .unwrap();
        captured.messages()
    }

    #[test]
    fn every_request_is_answered_and_tool_calls_run_beside_reads() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(temporary.path().join("Home.md"), "# Home\n").unwrap();
        let mut lines = vec![json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {}}})];
        for id in 1..=8 {
            lines.push(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "note_get", "arguments": {"note": "Home.md"}}}));
        }
        lines.push(json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list"}));
        let messages = run(temporary.path(), &lines);
        let mut ids = messages
            .iter()
            .filter_map(|message| message["id"].as_i64())
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, (0..=9).collect::<Vec<_>>());
        assert_eq!(messages[0]["result"]["protocolVersion"], "2025-11-25");
        assert!(messages
            .iter()
            .filter(|message| message["result"]["structuredContent"].is_object())
            .all(|message| message["result"]["isError"] == false));
    }

    #[test]
    fn a_cancelled_call_that_has_not_finished_gets_no_response() {
        let tracker = Tracker {
            calls: Mutex::new(Calls::default()),
            idle: Condvar::new(),
        };
        tracker.lock().pending.insert(id_key(&json!(7)));
        tracker.cancel(id_key(&json!(7)));
        assert!(tracker.is_cancelled(&id_key(&json!(7))));
        // Cancelling something not pending is ignored, so nothing piles up.
        tracker.cancel(id_key(&json!(8)));
        assert!(!tracker.is_cancelled(&id_key(&json!(8))));
        tracker.finish(&id_key(&json!(7)));
        assert!(tracker.lock().cancelled.is_empty());
        tracker.drain(Duration::from_millis(1));
    }

    #[test]
    fn malformed_lines_get_a_parse_error_and_reading_goes_on() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = VaultPaths::new(temporary.path());
        let core = McpServerCore::new(&paths, Some("readonly")).unwrap();
        let captured = Captured::default();
        serve(
            core,
            Duration::from_secs(30),
            Cursor::new("{not json\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n"),
            Box::new(captured.clone()),
        )
        .unwrap();
        let messages = captured.messages();
        assert_eq!(messages[0]["error"]["code"], -32700);
        assert_eq!(messages[1]["id"], 1);
    }
}
