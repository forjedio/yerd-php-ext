//! Loopback TCP transport: connect-once-per-request, non-blocking, fire-and-forget.
//!
//! Robustness rules: connect to `127.0.0.1:<port>` only,
//! with a tiny timeout; attempt the connect at most once per request; on any
//! failure, mark the connection dead for the rest of the request and silently
//! no-op. A down server must be invisible to the user's app.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::time::Duration;

/// Connect timeout — small enough to never perceptibly delay a request.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Per-request connection state. Held in thread-local request context.
pub enum Conn {
    /// Connect not yet attempted this request.
    Idle,
    /// Live connection to the dump server.
    Live(TcpStream),
    /// Connect failed this request; do not retry until next request.
    Dead,
}

impl Conn {
    /// Write one already-serialized, newline-terminated frame line.
    ///
    /// Lazily connects on first use. All errors are swallowed; a failed connect
    /// transitions to [`Conn::Dead`] so we attempt it at most once per request.
    pub fn send(&mut self, port: u16, line: &str) {
        if matches!(self, Conn::Idle) {
            *self = match Self::connect(port) {
                Some(stream) => Conn::Live(stream),
                None => Conn::Dead,
            };
        }

        if let Conn::Live(stream) = self {
            // Fire-and-forget. On a full kernel buffer (slow/absent reader) a
            // non-blocking `write_all` may emit a partial line then `WouldBlock`;
            // we mark the connection Dead so we never send a corrupt continuation
            // afterwards. Yerd drops the one unparseable line. Telemetry is
            // best-effort and must never stall the request.
            if stream.write_all(line.as_bytes()).is_err() {
                *self = Conn::Dead;
            }
        }
    }

    fn connect(port: u16) -> Option<TcpStream> {
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let stream = TcpStream::connect_timeout(&addr.into(), CONNECT_TIMEOUT).ok()?;
        // Non-blocking so a slow/full receiver can never block the FPM worker.
        stream.set_nonblocking(true).ok()?;
        let _ = stream.set_nodelay(true);
        Some(stream)
    }
}
