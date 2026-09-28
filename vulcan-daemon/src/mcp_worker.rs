//! Bounded synchronous MCP request workers shared by local and hosted adapters.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use vulcan_app::execution::ExecutionCancellationToken;

const MCP_REQUEST_WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum McpWorkerResult<T> {
    Completed(T),
    TimedOut,
    Disconnected,
    SpawnFailed,
}

/// Run one owned request on a dedicated stack, retaining the worker after a timeout.
///
/// The cancellation token is signaled on timeout, but callers must still treat an
/// already-dispatched mutation as having an unknown outcome until its durable
/// operation record reaches a terminal state.
pub fn run_mcp_worker<T, F>(
    name: &str,
    timeout: Duration,
    cancellation: Option<&ExecutionCancellationToken>,
    task: F,
) -> McpWorkerResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    if thread::Builder::new()
        .name(name.to_string())
        .stack_size(MCP_REQUEST_WORKER_STACK_SIZE)
        .spawn(move || {
            let _ = sender.send(task());
        })
        .is_err()
    {
        return McpWorkerResult::SpawnFailed;
    }
    match receiver.recv_timeout(timeout) {
        Ok(result) => McpWorkerResult::Completed(result),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            if let Some(cancellation) = cancellation {
                cancellation.cancel();
            }
            McpWorkerResult::TimedOut
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => McpWorkerResult::Disconnected,
    }
}

#[cfg(test)]
mod tests {
    use super::{run_mcp_worker, McpWorkerResult};
    use std::sync::mpsc;
    use std::time::Duration;
    use vulcan_app::execution::ExecutionCancellationToken;

    #[test]
    fn completed_worker_returns_owned_result_without_cancelling() {
        let cancellation = ExecutionCancellationToken::default();
        let result = run_mcp_worker(
            "mcp-worker-test",
            Duration::from_secs(1),
            Some(&cancellation),
            || 42,
        );
        assert_eq!(result, McpWorkerResult::Completed(42));
        assert!(!cancellation.is_cancelled());
    }

    #[test]
    fn timeout_signals_cancellation_while_worker_can_finish() {
        let cancellation = ExecutionCancellationToken::default();
        let (release, wait) = mpsc::channel::<()>();
        let (finished, observed) = mpsc::channel();
        let result = run_mcp_worker(
            "mcp-worker-timeout-test",
            Duration::from_millis(10),
            Some(&cancellation),
            move || {
                let _ = wait.recv();
                let _ = finished.send(());
            },
        );
        assert_eq!(result, McpWorkerResult::TimedOut);
        assert!(cancellation.is_cancelled());
        release.send(()).unwrap();
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn panicked_worker_reports_disconnect() {
        let result = run_mcp_worker::<(), _>(
            "mcp-worker-panic-test",
            Duration::from_secs(1),
            None,
            || panic!("worker failed"),
        );
        assert_eq!(result, McpWorkerResult::Disconnected);
    }
}
