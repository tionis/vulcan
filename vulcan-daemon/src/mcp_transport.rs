//! The HTTP listener shared by hosted MCP endpoints and other blocking HTTP
//! surfaces, such as previews, built on hyper.
//!
//! hyper owns HTTP framing, keep-alive and the header deadline. Each request is
//! read in full (bounded), handed to a synchronous handler on a blocking thread,
//! and answered through an [`McpHttpConnection`]: one complete response, or a
//! streamed body such as server-sent events. Authorization and protocol
//! dispatch stay in the handler.

use crate::mcp_http_codec::{
    McpHttpReadError, McpHttpRequest, McpHttpResponse, MAX_MCP_HTTP_BODY_BYTES,
};
use crate::shutdown::ShutdownSignal;
use axum::body::{Body, Bytes};
use axum::http::{HeaderName, HeaderValue, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::Value;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::io::{self, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Semaphore};
use vulcan_app::mcp_dispatch::jsonrpc_error;

/// The whole request, headers and body, must arrive within this time; it also
/// bounds how long an idle keep-alive connection waits for its next request.
const MCP_REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Requests handled at once; more are refused with 503.
const MAX_MCP_HTTP_REQUESTS: usize = 64;
/// Open connections, including idle keep-alive ones; more are closed at once.
const MAX_MCP_HTTP_CONNECTIONS: usize = 256;
/// How long a stopping listener lets running responses finish.
const MCP_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// Streamed body chunks buffered before the handler waits for the client.
const STREAM_BUFFER: usize = 16;

/// Headers whose repetition would make the request ambiguous.
const SINGLE_HEADERS: &[&str] = &[
    "content-length",
    "authorization",
    "origin",
    "host",
    "mcp-session-id",
    "mcp-protocol-version",
    "mcp-method",
    "mcp-name",
];

pub struct McpHttpListener {
    listener: TcpListener,
    address: SocketAddr,
    max_requests: usize,
}

impl McpHttpListener {
    pub fn bind(address: SocketAddr) -> io::Result<Self> {
        Self::bind_with_limit(address, MAX_MCP_HTTP_REQUESTS)
    }

    fn bind_with_limit(address: SocketAddr, max_requests: usize) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        Ok(Self {
            listener,
            address,
            max_requests,
        })
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Serve until `stop` is cancelled (forever without one), answering a
    /// request that could not be read with a JSON-RPC error.
    pub fn serve<F>(&self, stop: Option<&ShutdownSignal>, handler: F) -> io::Result<()>
    where
        F: Fn(&McpHttpRequest, &mut McpHttpConnection) + Send + Sync + 'static,
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
        F: Fn(&McpHttpRequest, &mut McpHttpConnection) + Send + Sync + 'static,
        E: Fn(&McpHttpReadError) -> McpHttpResponse + Send + Sync + 'static,
    {
        let listener = self.listener.try_clone()?;
        // tokio needs a non-blocking socket. A clone on Windows is a new
        // handle (WSADuplicateSocket) that does not carry the original's
        // mode, and a blocking accept never lets the loop see `stop`.
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            handler: Box::new(handler),
            malformed: Box::new(malformed),
            requests: Arc::new(Semaphore::new(self.max_requests)),
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("vulcan-http")
            .enable_all()
            .build()?;
        let result = runtime.block_on(accept_loop(listener, shared, stop));
        runtime.shutdown_timeout(MCP_SHUTDOWN_GRACE);
        result
    }
}

type Handler = dyn Fn(&McpHttpRequest, &mut McpHttpConnection) + Send + Sync;
type Malformed = dyn Fn(&McpHttpReadError) -> McpHttpResponse + Send + Sync;

struct Shared {
    handler: Box<Handler>,
    malformed: Box<Malformed>,
    requests: Arc<Semaphore>,
}

async fn accept_loop(
    listener: TcpListener,
    shared: Arc<Shared>,
    stop: Option<&ShutdownSignal>,
) -> io::Result<()> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let connections = Arc::new(Semaphore::new(MAX_MCP_HTTP_CONNECTIONS));
    let stopped = async {
        match stop {
            Some(stop) => stop.cancelled().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(stopped);
    loop {
        let (stream, _) = tokio::select! {
            () = &mut stopped => return Ok(()),
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                // A connection that failed before it was accepted (reset,
                // descriptor limits) must not stop the listener.
                Err(error) if is_transient_accept_error(&error) => continue,
                Err(error) => return Err(error),
            },
        };
        let Ok(slot) = Arc::clone(&connections).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let _slot = slot;
            let service = hyper::service::service_fn(move |request| {
                let shared = Arc::clone(&shared);
                async move { Ok::<_, Infallible>(handle(shared, request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(MCP_REQUEST_READ_TIMEOUT)
                .keep_alive(true)
                // A client may shut down its side after sending the request
                // and still read the response.
                .half_close(true)
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

fn is_transient_accept_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
    ) || error.raw_os_error().is_some_and(|code| {
        // EMFILE and ENFILE: out of descriptors for now.
        code == 24 || code == 23
    })
}

async fn handle(shared: Arc<Shared>, request: Request<hyper::body::Incoming>) -> Response<Body> {
    let Ok(permit) = Arc::clone(&shared.requests).try_acquire_owned() else {
        let mut response = to_response(McpHttpResponse {
            status: 503,
            content_type: Some("application/json"),
            body: br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":"MCP request limit reached"}}"#.to_vec(),
            extra_headers: vec![("Retry-After".to_string(), "1".to_string())],
        });
        response
            .headers_mut()
            .insert("connection", HeaderValue::from_static("close"));
        return response;
    };
    let request = match read_request(request).await {
        Ok(request) => request,
        Err(error) => {
            let mut response = to_response((shared.malformed)(&error));
            // The rest of a refused body may still be on the wire.
            response
                .headers_mut()
                .insert("connection", HeaderValue::from_static("close"));
            return response;
        }
    };
    let (head_sender, head_receiver) = oneshot::channel();
    let (body_sender, body_receiver) = mpsc::channel(STREAM_BUFFER);
    let worker = Arc::clone(&shared);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut connection = McpHttpConnection {
            head: Some(head_sender),
            body: Some(body_sender),
        };
        (worker.handler)(&request, &mut connection);
    });
    match head_receiver.await {
        Ok(Head::Complete(response)) => to_response(response),
        Ok(Head::Stream {
            status,
            content_type,
            headers,
        }) => {
            let stream = futures_util::stream::unfold(body_receiver, |mut receiver| async move {
                receiver
                    .recv()
                    .await
                    .map(|chunk| (Ok::<_, Infallible>(chunk), receiver))
            });
            let mut response = Response::new(Body::from_stream(stream));
            *response.status_mut() =
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            apply_headers(&mut response, Some(content_type), &headers);
            response
        }
        // The handler returned (or panicked) without answering.
        Err(_) => to_response(McpHttpResponse {
            status: 500,
            content_type: Some("application/json"),
            body: br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"the request ended without a response"}}"#.to_vec(),
            extra_headers: Vec::new(),
        }),
    }
}

async fn read_request(
    request: Request<hyper::body::Incoming>,
) -> Result<McpHttpRequest, McpHttpReadError> {
    let (parts, body) = request.into_parts();
    let mut headers = BTreeMap::new();
    for (name, value) in &parts.headers {
        let name = name.as_str().to_string();
        let value = value
            .to_str()
            .map_err(|_| McpHttpReadError::bad_request(format!("invalid `{name}` header value")))?
            .trim()
            .to_string();
        if headers.contains_key(&name) && SINGLE_HEADERS.contains(&name.as_str()) {
            return Err(McpHttpReadError::bad_request(format!(
                "duplicate HTTP request header: {name}"
            )));
        }
        headers.insert(name, value);
    }
    if let Some(length) = headers.get("content-length") {
        let length = length
            .parse::<usize>()
            .map_err(|_| McpHttpReadError::bad_request("invalid Content-Length"))?;
        if length > MAX_MCP_HTTP_BODY_BYTES {
            return Err(McpHttpReadError::payload_too_large());
        }
    }
    let body = tokio::time::timeout(
        MCP_REQUEST_READ_TIMEOUT,
        axum::body::to_bytes(Body::new(body), MAX_MCP_HTTP_BODY_BYTES),
    )
    .await
    .map_err(|_| McpHttpReadError::bad_request("MCP HTTP request deadline exceeded"))?
    .map_err(|error| {
        // `to_bytes` reports its limit only through the message.
        if error.to_string().contains("length limit exceeded") {
            McpHttpReadError::payload_too_large()
        } else {
            McpHttpReadError::bad_request("incomplete HTTP request body")
        }
    })?;
    Ok(McpHttpRequest {
        method: parts.method.as_str().to_string(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().unwrap_or_default().to_string(),
        headers,
        body: body.to_vec(),
        cancellation: vulcan_app::execution::ExecutionCancellationToken::default(),
    })
}

fn to_response(response: McpHttpResponse) -> Response<Body> {
    let mut built = Response::new(Body::from(response.body));
    *built.status_mut() =
        StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    apply_headers(&mut built, response.content_type, &response.extra_headers);
    built
}

fn apply_headers(
    response: &mut Response<Body>,
    content_type: Option<&'static str>,
    headers: &[(String, String)],
) {
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert("content-type", HeaderValue::from_static(content_type));
    }
    for (name, value) in headers {
        // Header names and values come from host code; an invalid one is
        // dropped rather than written.
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(name.as_str()),
            HeaderValue::try_from(value.as_str()),
        ) {
            response.headers_mut().append(name, value);
        }
    }
}

enum Head {
    Complete(McpHttpResponse),
    Stream {
        status: u16,
        content_type: &'static str,
        headers: Vec<(String, String)>,
    },
}

/// How a handler answers one request: [`Self::respond`] once, or
/// [`Self::start_stream`] and then write the body through [`Write`]. A write
/// fails once the client has gone away.
pub struct McpHttpConnection {
    head: Option<oneshot::Sender<Head>>,
    body: Option<mpsc::Sender<Bytes>>,
}

impl McpHttpConnection {
    /// Send one complete response.
    pub fn respond(&mut self, response: McpHttpResponse) -> io::Result<()> {
        self.body = None;
        self.send_head(Head::Complete(response))
    }

    /// Start a streamed `200` response; its body is what is then written.
    pub fn start_stream(
        &mut self,
        content_type: &'static str,
        headers: Vec<(String, String)>,
    ) -> io::Result<()> {
        self.send_head(Head::Stream {
            status: 200,
            content_type,
            headers,
        })
    }

    /// Start a `text/event-stream` response that proxies must not buffer.
    pub fn start_event_stream(&mut self) -> io::Result<()> {
        self.start_stream(
            "text/event-stream",
            vec![
                ("Cache-Control".to_string(), "no-cache".to_string()),
                ("X-Accel-Buffering".to_string(), "no".to_string()),
            ],
        )
    }

    fn send_head(&mut self, head: Head) -> io::Result<()> {
        self.head
            .take()
            .ok_or_else(|| io::Error::other("the response was already started"))?
            .send(head)
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

#[cfg(test)]
impl McpHttpConnection {
    /// A connection without a client, for handler tests: the capture returns
    /// what the handler answered.
    pub(crate) fn for_test() -> (Self, McpHttpTestCapture) {
        let (head, head_receiver) = oneshot::channel();
        let (body, body_receiver) = mpsc::channel(1024);
        (
            Self {
                head: Some(head),
                body: Some(body),
            },
            McpHttpTestCapture {
                head: head_receiver,
                body: body_receiver,
            },
        )
    }
}

#[cfg(test)]
pub(crate) struct McpHttpTestCapture {
    head: oneshot::Receiver<Head>,
    body: mpsc::Receiver<Bytes>,
}

#[cfg(test)]
impl McpHttpTestCapture {
    /// The response as status, content type and body; a stream's body is what
    /// was written to it so far.
    pub(crate) fn response(mut self) -> (u16, Option<&'static str>, String) {
        let (status, content_type, mut body) = match self.head.try_recv() {
            Ok(Head::Complete(response)) => (
                response.status,
                response.content_type,
                String::from_utf8_lossy(&response.body).into_owned(),
            ),
            Ok(Head::Stream {
                status,
                content_type,
                ..
            }) => (status, Some(content_type), String::new()),
            Err(error) => panic!("the handler did not answer: {error}"),
        };
        while let Ok(chunk) = self.body.try_recv() {
            body.push_str(&String::from_utf8_lossy(&chunk));
        }
        (status, content_type, body)
    }
}

impl Write for McpHttpConnection {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.head.is_some() {
            return Err(io::Error::other("the stream was not started"));
        }
        let body = self
            .body
            .as_ref()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        body.blocking_send(Bytes::copy_from_slice(buffer))
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpStream;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc as std_mpsc;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Instant;

    const TEST_REQUEST: &[u8] =
        b"GET /mcp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

    fn ok(body: &str) -> McpHttpResponse {
        McpHttpResponse {
            status: 200,
            content_type: Some("text/plain"),
            body: body.as_bytes().to_vec(),
            extra_headers: Vec::new(),
        }
    }

    fn start(
        listener: McpHttpListener,
        handler: impl Fn(&McpHttpRequest, &mut McpHttpConnection) + Send + Sync + 'static,
    ) -> (
        SocketAddr,
        Arc<ShutdownSignal>,
        thread::JoinHandle<io::Result<()>>,
    ) {
        let address = listener.local_addr();
        let stop = Arc::new(ShutdownSignal::new(false));
        let runner_stop = Arc::clone(&stop);
        let runner = thread::spawn(move || listener.serve(Some(&runner_stop), handler));
        (address, stop, runner)
    }

    fn exchange(address: SocketAddr, request: &[u8]) -> String {
        let mut client = TcpStream::connect(address).expect("connect");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        client.write_all(request).expect("send request");
        // Half-closed clients still get their response.
        let _ = client.shutdown(std::net::Shutdown::Write);
        let mut response = String::new();
        client.read_to_string(&mut response).expect("response");
        response
    }

    #[test]
    fn listener_dispatches_requests_and_stops_on_signal() {
        let listener = McpHttpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let (sender, receiver) = std_mpsc::channel();
        let sender = Mutex::new(sender);
        let (address, stop, runner) = start(listener, move |request, connection| {
            sender
                .lock()
                .unwrap()
                .send((request.path.clone(), request.query.clone()))
                .unwrap();
            connection.respond(ok("R")).unwrap();
        });
        let response = exchange(
            address,
            b"GET /mcp?x=1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(response.ends_with("\r\n\r\nR"));
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            ("/mcp".to_string(), "x=1".to_string())
        );
        stop.cancel();
        runner.join().unwrap().unwrap();
        assert!(TcpStream::connect(address).is_err() || exchange_fails(address));
    }

    fn exchange_fails(address: SocketAddr) -> bool {
        let Ok(mut client) = TcpStream::connect(address) else {
            return true;
        };
        let _ = client.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = client.write_all(TEST_REQUEST);
        let mut response = String::new();
        client.read_to_string(&mut response).is_err() || response.is_empty()
    }

    #[test]
    fn keep_alive_serves_several_requests_on_one_connection() {
        let listener = McpHttpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let (address, stop, runner) = start(listener, |request, connection| {
            connection.respond(ok(&request.path)).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(client.try_clone().unwrap());
        for path in ["/one", "/two"] {
            write!(client, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(body, path.as_bytes());
        }
        stop.cancel();
        runner.join().unwrap().unwrap();
    }

    #[test]
    fn unreadable_requests_are_rejected_before_the_handler() {
        let listener = McpHttpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let runner_calls = Arc::clone(&calls);
        let (address, stop, runner) = start(listener, move |_request, connection| {
            runner_calls.fetch_add(1, Ordering::SeqCst);
            connection.respond(ok("R")).unwrap();
        });
        let oversized = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            MAX_MCP_HTTP_BODY_BYTES + 1
        );
        let response = exchange(address, oversized.as_bytes());
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        let duplicate = exchange(
            address,
            b"POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer one\r\nAuthorization: Bearer two\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(duplicate.starts_with("HTTP/1.1 400"), "{duplicate}");
        let body: Value =
            serde_json::from_str(duplicate.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["error"]["code"], -32600);
        let malformed = exchange(address, b"NOT HTTP\r\n\r\n");
        assert!(malformed.starts_with("HTTP/1.1 400"), "{malformed}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        stop.cancel();
        runner.join().unwrap().unwrap();
    }

    #[test]
    fn a_stalled_request_times_out() {
        let listener = McpHttpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let (address, stop, runner) = start(listener, |_request, connection| {
            connection.respond(ok("R")).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(MCP_REQUEST_READ_TIMEOUT * 2))
            .unwrap();
        client.write_all(b"GET /mcp HTTP/1.1\r\nHost: loc").unwrap();
        let started = Instant::now();
        let mut response = Vec::new();
        let _ = client.read_to_end(&mut response);
        assert!(started.elapsed() < MCP_REQUEST_READ_TIMEOUT + Duration::from_secs(3));
        stop.cancel();
        runner.join().unwrap().unwrap();
    }

    #[test]
    fn excess_requests_are_refused_and_finished_ones_release_their_slot() {
        let listener = McpHttpListener::bind_with_limit("127.0.0.1:0".parse().unwrap(), 1).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let runner_calls = Arc::clone(&calls);
        let (started_sender, started_receiver) = std_mpsc::channel();
        let (release_sender, release_receiver) = std_mpsc::channel::<()>();
        let started_sender = Mutex::new(started_sender);
        let release_receiver = Mutex::new(release_receiver);
        let (address, stop, runner) = start(listener, move |_request, connection| {
            if runner_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                started_sender.lock().unwrap().send(()).unwrap();
                release_receiver
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            connection.respond(ok("R")).unwrap();
        });
        let first = thread::spawn(move || exchange(address, TEST_REQUEST));
        started_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let rejected = exchange(address, TEST_REQUEST);
        assert!(
            rejected.starts_with("HTTP/1.1 503 Service Unavailable"),
            "{rejected}"
        );
        assert!(rejected.to_ascii_lowercase().contains("retry-after: 1\r\n"));
        release_sender.send(()).unwrap();
        assert!(first.join().unwrap().ends_with("\r\n\r\nR"));
        assert!(exchange(address, TEST_REQUEST).ends_with("\r\n\r\nR"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        stop.cancel();
        runner.join().unwrap().unwrap();
    }

    #[test]
    fn a_panicking_handler_answers_500_and_releases_its_slot() {
        let listener = McpHttpListener::bind_with_limit("127.0.0.1:0".parse().unwrap(), 1).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let runner_calls = Arc::clone(&calls);
        let (address, stop, runner) = start(listener, move |_request, connection| {
            assert_ne!(
                runner_calls.fetch_add(1, Ordering::SeqCst),
                0,
                "simulated handler panic"
            );
            connection.respond(ok("R")).unwrap();
        });
        assert!(exchange(address, TEST_REQUEST).starts_with("HTTP/1.1 500"));
        assert!(exchange(address, TEST_REQUEST).ends_with("\r\n\r\nR"));
        stop.cancel();
        runner.join().unwrap().unwrap();
    }

    #[test]
    fn streams_deliver_writes_and_report_a_departed_client() {
        let listener = McpHttpListener::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let (ended_sender, ended_receiver) = std_mpsc::channel();
        let ended_sender = Mutex::new(ended_sender);
        let (address, stop, runner) = start(listener, move |_request, connection| {
            connection.start_event_stream().unwrap();
            connection.write_all(b"data: first\n\n").unwrap();
            let failed = (0..500).any(|_| {
                thread::sleep(Duration::from_millis(10));
                connection.write_all(b": keepalive\n\n").is_err()
            });
            ended_sender.lock().unwrap().send(failed).unwrap();
        });
        let mut client = TcpStream::connect(address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(TEST_REQUEST).unwrap();
        let mut reader = BufReader::new(client);
        let mut seen = String::new();
        while !seen.contains("data: first") {
            reader.read_line(&mut seen).unwrap();
        }
        let head = seen.to_ascii_lowercase();
        assert!(head.contains("content-type: text/event-stream"));
        assert!(head.contains("x-accel-buffering: no"));
        drop(reader);
        assert!(ended_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap());
        stop.cancel();
        runner.join().unwrap().unwrap();
    }
}
