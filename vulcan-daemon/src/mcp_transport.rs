//! Transport-only TCP lifecycle for hosted MCP HTTP listeners.
//!
//! Authorization and protocol dispatch remain in the supplied connection
//! handler. This boundary lets foreground and resident hosts use the same
//! bind, accept, shutdown, and per-connection timeout behavior.

use crate::mcp_http_codec::{write_mcp_http_response, McpHttpResponse};
use crate::shutdown::ShutdownSignal;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const MCP_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MCP_CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MCP_HTTP_CONNECTIONS: usize = 64;

pub struct McpHttpListener {
    listener: TcpListener,
    address: SocketAddr,
    active_connections: Arc<AtomicUsize>,
    max_connections: usize,
}

impl McpHttpListener {
    pub fn bind(address: SocketAddr) -> io::Result<Self> {
        Self::bind_with_limit(address, MAX_MCP_HTTP_CONNECTIONS)
    }

    fn bind_with_limit(address: SocketAddr, max_connections: usize) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        Ok(Self {
            listener,
            address,
            active_connections: Arc::new(AtomicUsize::new(0)),
            max_connections,
        })
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub fn serve<F>(&self, stop: Option<&ShutdownSignal>, handler: F) -> io::Result<()>
    where
        F: Fn(&mut TcpStream) + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        loop {
            if stop.is_some_and(ShutdownSignal::is_cancelled) {
                return Ok(());
            }
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let active = Arc::clone(&self.active_connections);
                    if active
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                            (count < self.max_connections).then_some(count + 1)
                        })
                        .is_err()
                    {
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                        let _ = write_mcp_http_response(
                            &mut stream,
                            &McpHttpResponse {
                                status: 503,
                                content_type: Some("application/json"),
                                body: br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":"MCP connection limit reached"}}"#.to_vec(),
                                extra_headers: vec![("Retry-After".to_string(), "1".to_string())],
                            },
                        );
                        continue;
                    }
                    let handler = Arc::clone(&handler);
                    let slot = ConnectionSlot(active);
                    thread::Builder::new()
                        .name("vulcan-mcp-http-connection".to_string())
                        .spawn(move || {
                            let _slot = slot;
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(MCP_CONNECTION_READ_TIMEOUT));
                            handler(&mut stream);
                        })?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if let Some(stop) = stop {
                        stop.wait_timeout(MCP_ACCEPT_POLL_INTERVAL);
                    } else {
                        thread::sleep(MCP_ACCEPT_POLL_INTERVAL);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
}

struct ConnectionSlot(Arc<AtomicUsize>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::mpsc;
    use std::sync::Mutex;

    #[test]
    fn listener_dispatches_connections_and_stops_on_signal() {
        let listener =
            McpHttpListener::bind("127.0.0.1:0".parse().expect("address")).expect("listener");
        let address = listener.local_addr();
        let stop = Arc::new(ShutdownSignal::new(false));
        let runner_stop = Arc::clone(&stop);
        let (sender, receiver) = mpsc::channel();
        let runner = thread::spawn(move || {
            listener.serve(Some(&runner_stop), move |stream| {
                let mut byte = [0];
                stream.read_exact(&mut byte).expect("request byte");
                sender.send(byte[0]).expect("request receiver");
                stream.write_all(b"R").expect("response byte");
            })
        });
        let mut client = TcpStream::connect(address).expect("connect");
        client.write_all(b"Q").expect("send request");
        let mut response = [0];
        client.read_exact(&mut response).expect("response");
        assert_eq!(response, *b"R");
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("dispatch"),
            b'Q'
        );
        stop.cancel();
        runner.join().expect("listener thread").expect("shutdown");
        assert!(TcpStream::connect(address).is_err());
    }

    #[test]
    fn listener_rejects_excess_connections_and_releases_finished_slots() {
        let listener = McpHttpListener::bind_with_limit("127.0.0.1:0".parse().expect("address"), 1)
            .expect("listener");
        let address = listener.local_addr();
        let active = Arc::clone(&listener.active_connections);
        let stop = Arc::new(ShutdownSignal::new(false));
        let runner_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let runner_calls = Arc::clone(&calls);
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let release_receiver = Arc::new(Mutex::new(release_receiver));
        let runner = thread::spawn(move || {
            listener.serve(Some(&runner_stop), move |stream| {
                if runner_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    started_sender.send(()).expect("started receiver");
                    release_receiver
                        .lock()
                        .expect("release lock")
                        .recv_timeout(Duration::from_secs(2))
                        .expect("release first connection");
                }
                stream.write_all(b"R").expect("response");
            })
        });
        let mut first = TcpStream::connect(address).expect("first connection");
        started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("first handler started");
        let mut second = TcpStream::connect(address).expect("second connection");
        second
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        let mut rejected = String::new();
        second
            .read_to_string(&mut rejected)
            .expect("rejection response");
        assert!(rejected.starts_with("HTTP/1.1 503 Service Unavailable"));
        assert!(rejected.contains("Retry-After: 1\r\n"));
        assert!(rejected.contains("MCP connection limit reached"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release_sender.send(()).expect("release first handler");
        let mut response = [0];
        first.read_exact(&mut response).expect("first response");
        assert_eq!(response, *b"R");
        for _ in 0..100 {
            if active.load(Ordering::Acquire) == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(active.load(Ordering::Acquire), 0);
        let mut third = TcpStream::connect(address).expect("third connection");
        third.read_exact(&mut response).expect("third response");
        assert_eq!(response, *b"R");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        stop.cancel();
        runner.join().expect("listener thread").expect("shutdown");
    }

    #[test]
    fn panicking_connection_worker_releases_its_slot() {
        let listener = McpHttpListener::bind_with_limit("127.0.0.1:0".parse().expect("address"), 1)
            .expect("listener");
        let address = listener.local_addr();
        let active = Arc::clone(&listener.active_connections);
        let stop = Arc::new(ShutdownSignal::new(false));
        let runner_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let runner_calls = Arc::clone(&calls);
        let runner = thread::spawn(move || {
            listener.serve(Some(&runner_stop), move |stream| {
                assert_ne!(
                    runner_calls.fetch_add(1, Ordering::SeqCst),
                    0,
                    "simulated handler panic"
                );
                stream.write_all(b"R").expect("response");
            })
        });
        let _first = TcpStream::connect(address).expect("first connection");
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) == 1 && active.load(Ordering::Acquire) == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::Acquire), 0);
        let mut second = TcpStream::connect(address).expect("second connection");
        let mut response = [0];
        second.read_exact(&mut response).expect("second response");
        assert_eq!(response, *b"R");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        stop.cancel();
        runner.join().expect("listener thread").expect("shutdown");
    }
}
