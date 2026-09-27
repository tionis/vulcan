//! Transport-only TCP lifecycle for hosted MCP HTTP listeners.
//!
//! Authorization and protocol dispatch remain in the supplied connection
//! handler. This boundary lets foreground and resident hosts use the same
//! bind, accept, shutdown, and per-connection timeout behavior.

use crate::shutdown::ShutdownSignal;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const MCP_ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MCP_CONNECTION_READ_TIMEOUT: Duration = Duration::from_secs(5);

pub struct McpHttpListener {
    listener: TcpListener,
    address: SocketAddr,
}

impl McpHttpListener {
    pub fn bind(address: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        Ok(Self { listener, address })
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
                    let handler = Arc::clone(&handler);
                    thread::spawn(move || {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(MCP_CONNECTION_READ_TIMEOUT));
                        handler(&mut stream);
                    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::mpsc;

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
}
