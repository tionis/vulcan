//! Progress notifications for a request that asked for them with
//! `_meta.progressToken` (both protocol eras): the host gives the core a
//! notifier that delivers a message on the request's own stream, and long
//! tools report through [`McpProgress`].

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Delivers one notification on the stream of the request being served.
#[derive(Clone)]
pub struct McpNotifier(pub Arc<dyn Fn(Value) + Send + Sync>);

impl std::fmt::Debug for McpNotifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("McpNotifier")
    }
}

/// The fewest milliseconds between two progress notifications, apart from
/// the last.
const MIN_INTERVAL: Duration = Duration::from_millis(250);

/// Progress for one request.
#[derive(Debug)]
pub struct McpProgress {
    token: Value,
    notifier: McpNotifier,
    state: Mutex<ProgressState>,
}

#[derive(Debug)]
struct ProgressState {
    last_sent: Option<Instant>,
    last_progress: f64,
}

impl McpProgress {
    /// Progress for a request whose `_meta` carries a progress token, when
    /// the host can deliver notifications.
    #[must_use]
    pub fn for_request(params: Option<&Value>, notifier: Option<&McpNotifier>) -> Option<Self> {
        let token = params?.get("_meta")?.get("progressToken")?;
        if !(token.is_string() || token.is_i64() || token.is_u64()) {
            return None;
        }
        Some(Self {
            token: token.clone(),
            notifier: notifier?.clone(),
            state: Mutex::new(ProgressState {
                last_sent: None,
                last_progress: -1.0,
            }),
        })
    }

    /// Report `progress` (of `total`, when known). Progress only increases,
    /// so a report that does not is dropped, and reports closer together
    /// than 250 ms are dropped unless they finish the work.
    pub fn report(&self, progress: f64, total: Option<f64>, message: Option<&str>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if progress <= state.last_progress {
            return;
        }
        let finished = total.is_some_and(|total| progress >= total);
        if !finished
            && state
                .last_sent
                .is_some_and(|sent| sent.elapsed() < MIN_INTERVAL)
        {
            return;
        }
        state.last_sent = Some(Instant::now());
        state.last_progress = progress;
        drop(state);
        let mut params = json!({ "progressToken": self.token, "progress": progress });
        if let Some(total) = total {
            params["total"] = json!(total);
        }
        if let Some(message) = message {
            params["message"] = Value::from(message);
        }
        (self.notifier.0)(json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": params,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collecting() -> (McpNotifier, Arc<Mutex<Vec<Value>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&sent);
        (
            McpNotifier(Arc::new(move |message| sink.lock().unwrap().push(message))),
            sent,
        )
    }

    #[test]
    fn progress_needs_a_token_and_a_notifier() {
        let (notifier, _) = collecting();
        let with_token = json!({"_meta": {"progressToken": "abc"}});
        assert!(McpProgress::for_request(Some(&with_token), Some(&notifier)).is_some());
        assert!(McpProgress::for_request(Some(&with_token), None).is_none());
        assert!(McpProgress::for_request(Some(&json!({"_meta": {}})), Some(&notifier)).is_none());
        let bad = json!({"_meta": {"progressToken": {"no": 1}}});
        assert!(McpProgress::for_request(Some(&bad), Some(&notifier)).is_none());
    }

    #[test]
    fn progress_increases_is_rate_limited_and_always_sends_the_end() {
        let (notifier, sent) = collecting();
        let params = json!({"_meta": {"progressToken": 7}});
        let progress = McpProgress::for_request(Some(&params), Some(&notifier)).unwrap();
        progress.report(1.0, Some(10.0), Some("Scanning"));
        progress.report(2.0, Some(10.0), None); // too soon
        progress.report(1.0, Some(10.0), None); // not an increase
        progress.report(10.0, Some(10.0), Some("Done")); // the end
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(
            sent[0],
            json!({"jsonrpc": "2.0", "method": "notifications/progress",
                   "params": {"progressToken": 7, "progress": 1.0, "total": 10.0, "message": "Scanning"}})
        );
        assert_eq!(sent[1]["params"]["progress"], 10.0);
        assert_eq!(sent[1]["params"]["message"], "Done");
    }
}
