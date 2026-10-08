//! Transport-only TCP lifecycle for hosted MCP HTTP listeners and other
//! blocking HTTP surfaces, such as previews.
//!
//! Authorization and protocol dispatch remain in the supplied connection
//! handler. This boundary lets foreground and resident hosts use the same
//! bind, accept, shutdown, and per-connection timeout behavior.

use crate::mcp_http_codec::{
    read_mcp_http_request, write_mcp_http_response, McpHttpReadError, McpHttpRequest,
    McpHttpResponse,
};
use crate::shutdown::ShutdownSignal;
use serde_json::Value;
use std::io::{self, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use vulcan_app::mcp_dispatch::jsonrpc_error;

const MCP_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MCP_CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(5);
const MCP_CONNECTION_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
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
        F: Fn(&McpHttpRequest, &mut TcpStream) + Send + Sync + 'static,
    {
        self.serve_with_errors(stop, handler, |error| {
            let body = jsonrpc_error(Value::Null, -32600, error.message.clone(), None);
            McpHttpResponse {
                status: error.status,
                content_type: Some("application/json"),
                body: serde_json::to_vec(&body).expect("JSON-RPC error should serialize"),
                extra_headers: Vec::new(),
            }
        })
    }

    /// [`Self::serve`] for any HTTP surface: `malformed` renders the response
    /// to a request that could not be read.
    pub fn serve_with_errors<F, E>(
        &self,
        stop: Option<&ShutdownSignal>,
        handler: F,
        malformed: E,
    ) -> io::Result<()>
    where
        F: Fn(&McpHttpRequest, &mut TcpStream) + Send + Sync + 'static,
        E: Fn(&McpHttpReadError) -> McpHttpResponse + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        let malformed = Arc::new(malformed);
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
                    let malformed = Arc::clone(&malformed);
                    let slot = ConnectionSlot(active);
                    thread::Builder::new()
                        .name("vulcan-mcp-http-connection".to_string())
                        .spawn(move || {
                            let _slot = slot;
                            let _ = stream.set_nonblocking(false);
                            if stream
                                .set_write_timeout(Some(MCP_CONNECTION_WRITE_TIMEOUT))
                                .is_err()
                            {
                                return;
                            }
                            let mut reader = RequestDeadlineReader::new(
                                &mut stream,
                                MCP_CONNECTION_READ_TIMEOUT,
                            );
                            match read_mcp_http_request(&mut reader) {
                                Ok(request) => handler(&request, &mut stream),
                                Err(error) => {
                                    let _ =
                                        write_mcp_http_response(&mut stream, &malformed(&error));
                                }
                            }
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

/// A deadline for the whole request, not a timeout refreshed by every byte.
struct RequestDeadlineReader<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl<'a> RequestDeadlineReader<'a> {
    fn new(stream: &'a mut TcpStream, timeout: Duration) -> Self {
        Self {
            stream,
            deadline: Instant::now() + timeout,
        }
    }
}

impl Read for RequestDeadlineReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP HTTP request deadline exceeded",
            ));
        }
        self.stream
            .set_read_timeout(Some(remaining.max(Duration::from_millis(1))))?;
        let read = self.stream.read(buffer)?;
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP HTTP request deadline exceeded",
            ));
        }
        Ok(read)
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

    const TEST_REQUEST: &[u8] =
        b"GET /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n";

    #[test]
    fn request_deadline_expires_even_when_a_client_keeps_sending_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let mut client =
            TcpStream::connect(listener.local_addr().expect("address")).expect("client connection");
        let (mut server, _) = listener.accept().expect("server connection");
        let writer = thread::spawn(move || {
            for _ in 0..30 {
                if client.write_all(b"x").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        let error = {
            let mut reader = RequestDeadlineReader::new(&mut server, Duration::from_millis(100));
            read_mcp_http_request(&mut reader).expect_err("slow request must time out")
        };
        assert_eq!(error.status, 400);
        assert!(started.elapsed() < Duration::from_millis(500));
        drop(server);
        writer.join().expect("writer thread");
    }

    #[test]
    fn listener_dispatches_connections_and_stops_on_signal() {
        let listener =
            McpHttpListener::bind("127.0.0.1:0".parse().expect("address")).expect("listener");
        let address = listener.local_addr();
        let stop = Arc::new(ShutdownSignal::new(false));
        let runner_stop = Arc::clone(&stop);
        let (sender, receiver) = mpsc::channel();
        let runner = thread::spawn(move || {
            listener.serve(Some(&runner_stop), move |request, stream| {
                sender
                    .send((
                        request.path.clone(),
                        stream.write_timeout().expect("write timeout"),
                    ))
                    .expect("request receiver");
                stream.write_all(b"R").expect("response byte");
            })
        });
        let mut client = TcpStream::connect(address).expect("connect");
        client.write_all(TEST_REQUEST).expect("send request");
        let mut response = [0];
        client.read_exact(&mut response).expect("response");
        assert_eq!(response, *b"R");
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("dispatch"),
            ("/mcp".to_string(), Some(MCP_CONNECTION_WRITE_TIMEOUT))
        );
        stop.cancel();
        runner.join().expect("listener thread").expect("shutdown");
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn malformed_request_is_rejected_before_the_route_handler() {
        let listener =
            McpHttpListener::bind("127.0.0.1:0".parse().expect("address")).expect("listener");
        let address = listener.local_addr();
        let stop = Arc::new(ShutdownSignal::new(false));
        let runner_stop = Arc::clone(&stop);
        let calls = Arc::new(AtomicUsize::new(0));
        let runner_calls = Arc::clone(&calls);
        let runner = thread::spawn(move || {
            listener.serve(Some(&runner_stop), move |_request, _stream| {
                runner_calls.fetch_add(1, Ordering::SeqCst);
            })
        });
        let mut client = TcpStream::connect(address).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        client
            .write_all(b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: nope\r\n\r\n")
            .expect("malformed request");
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("error response");
        assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
        let body: Value =
            serde_json::from_str(response.split_once("\r\n\r\n").expect("response body").1)
                .expect("JSON-RPC response");
        assert_eq!(body["error"]["code"], -32600);
        assert_eq!(body["id"], Value::Null);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        stop.cancel();
        runner.join().expect("listener thread").expect("shutdown");
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
            listener.serve(Some(&runner_stop), move |_request, stream| {
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
        first.write_all(TEST_REQUEST).expect("first request");
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
        third.write_all(TEST_REQUEST).expect("third request");
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
            listener.serve(Some(&runner_stop), move |_request, stream| {
                assert_ne!(
                    runner_calls.fetch_add(1, Ordering::SeqCst),
                    0,
                    "simulated handler panic"
                );
                stream.write_all(b"R").expect("response");
            })
        });
        let mut first = TcpStream::connect(address).expect("first connection");
        first.write_all(TEST_REQUEST).expect("first request");
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) == 1 && active.load(Ordering::Acquire) == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::Acquire), 0);
        let mut second = TcpStream::connect(address).expect("second connection");
        second.write_all(TEST_REQUEST).expect("second request");
        let mut response = [0];
        second.read_exact(&mut response).expect("second response");
        assert_eq!(response, *b"R");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        stop.cancel();
        runner.join().expect("listener thread").expect("shutdown");
    }
}
