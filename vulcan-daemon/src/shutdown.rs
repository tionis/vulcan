//! Wakeable cancellation shared by synchronous workers and async listeners.

use std::sync::{Condvar, Mutex};
use std::thread::Thread;
use std::time::Duration;

#[derive(Debug, Default)]
struct State {
    cancelled: bool,
    parked: Vec<Thread>,
}

#[derive(Debug)]
pub struct ShutdownSignal {
    state: Mutex<State>,
    wake: Condvar,
    asynchronous: tokio::sync::watch::Sender<bool>,
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new(false)
    }
}

impl ShutdownSignal {
    #[must_use]
    pub fn new(cancelled: bool) -> Self {
        Self {
            state: Mutex::new(State {
                cancelled,
                parked: Vec::new(),
            }),
            wake: Condvar::new(),
            asynchronous: tokio::sync::watch::channel(cancelled).0,
        }
    }

    pub fn cancel(&self) {
        let mut state = self.state.lock().expect("shutdown lock");
        state.cancelled = true;
        for thread in &state.parked {
            thread.unpark();
        }
        self.wake.notify_all();
        self.asynchronous.send_replace(true);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.lock().expect("shutdown lock").cancelled
    }

    /// Returns true when cancelled, false when the actual work deadline expires.
    pub fn wait_timeout(&self, duration: Duration) -> bool {
        let state = self.state.lock().expect("shutdown lock");
        let (state, _) = self
            .wake
            .wait_timeout_while(state, duration, |state| !state.cancelled)
            .expect("shutdown wait");
        state.cancelled
    }

    pub async fn cancelled(&self) {
        let mut receiver = self.asynchronous.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    /// A watcher owns its signal for the lifetime of this registered thread.
    pub(crate) fn register_current_thread(&self) {
        let mut state = self.state.lock().expect("shutdown lock");
        let thread = std::thread::current();
        if state.cancelled {
            thread.unpark();
        }
        if !state
            .parked
            .iter()
            .any(|registered| registered.id() == thread.id())
        {
            state.parked.push(thread);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};

    #[tokio::test]
    async fn cancellation_wakes_blocking_async_and_parked_waiters() {
        let signal = Arc::new(ShutdownSignal::default());
        let thread_signal = Arc::clone(&signal);
        let (ready, receiver) = mpsc::channel();
        let parked = std::thread::spawn(move || {
            thread_signal.register_current_thread();
            ready.send(()).unwrap();
            while !thread_signal.is_cancelled() {
                std::thread::park();
            }
        });
        receiver.recv().unwrap();
        let blocking_signal = Arc::clone(&signal);
        let blocking = std::thread::spawn(move || {
            assert!(blocking_signal.wait_timeout(Duration::from_secs(60)));
        });
        signal.cancel();
        tokio::time::timeout(Duration::from_secs(1), signal.cancelled())
            .await
            .unwrap();
        parked.join().unwrap();
        blocking.join().unwrap();
        assert!(signal.wait_timeout(Duration::from_secs(60)));
        signal.cancel();
    }

    #[test]
    fn idle_wait_reaches_deadline_without_cancelling() {
        let signal = ShutdownSignal::default();
        let duration = Duration::from_millis(20);
        let start = std::time::Instant::now();
        assert!(!signal.wait_timeout(duration));
        assert!(start.elapsed() >= duration);
        assert!(!signal.is_cancelled());
    }
}
