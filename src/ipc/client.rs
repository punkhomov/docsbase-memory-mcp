//! Socket-first CLI client (FR-30, design §6).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::ipc::protocol::{self, PROTOCOL_VERSION, Request, Response};

/// Timeout for a single request/response exchange.
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Connected daemon client.
pub struct Client {
    stream: BufReader<UnixStream>,
}

impl Client {
    /// Connects to `$CACHE/state/daemon.sock`.
    ///
    /// Returns `None` when no daemon is listening (missing or stale socket),
    /// which callers treat as "fall back to direct mode".
    #[must_use]
    pub fn connect(cache: &Path) -> Option<Self> {
        let path = socket_path(cache);
        if !path.exists() {
            return None;
        }
        let stream = UnixStream::connect(path).ok()?;
        stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(IO_TIMEOUT)).ok()?;
        Some(Self {
            stream: BufReader::new(stream),
        })
    }

    /// Adjusts the read timeout for one exchange (long tools need longer).
    ///
    /// # Errors
    /// Returns [`Error::Internal`] when the socket refuses the option.
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.stream
            .get_mut()
            .set_read_timeout(timeout)
            .map_err(io_error)
    }

    /// Sends one request and reads one NDJSON response.
    ///
    /// # Errors
    /// Returns [`Error::Protocol`] on framing/decoding problems and
    /// [`Error::Internal`] on socket IO failures.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "requests are consumed once sent; keeps the brief's `call(Request)` contract"
    )]
    pub fn call(&mut self, request: Request) -> Result<Response> {
        let bytes = protocol::encode(&request)?;
        let stream = self.stream.get_mut();
        stream.write_all(&bytes).map_err(io_error)?;
        stream.flush().map_err(io_error)?;

        let mut line = Vec::new();
        let read = self.stream.read_until(b'\n', &mut line).map_err(io_error)?;
        if read == 0 {
            return Err(Error::transport("connection closed"));
        }
        protocol::decode_response(&line)
            .map_err(|err| Error::transport(format!("bad response: {err}")))
    }

    /// Performs the `Hello` exchange only; used by registry-wide tools that
    /// need no project binding (FR-26).
    ///
    /// # Errors
    /// Returns protocol/admission errors on version or build mismatch.
    pub fn handshake_registry(&mut self) -> Result<()> {
        let hello = self.call(Request::Hello {
            protocol_version: PROTOCOL_VERSION,
            build_id: protocol::build_id(),
            client: "docsbase-cli".to_owned(),
        })?;
        match hello {
            Response::Hello {
                protocol_version, ..
            } => protocol::ensure_protocol_version(protocol_version),
            Response::Error { code, message } => Err(Error::from_mcp_code(code, message)),
            other => Err(Error::Protocol {
                message: format!("unexpected hello reply: {other:?}"),
            }),
        }
    }

    /// Performs `Hello` + `RegisterSession` for `cwd` (FR-7, I6).
    ///
    /// # Errors
    /// Returns protocol/admission errors on version mismatch or an
    /// unregistered project reply.
    pub fn handshake(&mut self, cwd: &Path) -> Result<()> {
        self.handshake_registry()?;

        match self.call(Request::RegisterSession {
            pid: std::process::id(),
            cwd: cwd.to_path_buf(),
        })? {
            Response::ToolResult { .. } => Ok(()),
            Response::Error { code, message } => Err(Error::from_mcp_code(code, message)),
            other => Err(Error::Protocol {
                message: format!("unexpected session reply: {other:?}"),
            }),
        }
    }

    /// Sends a `CallTool` request and returns the tool payload.
    ///
    /// # Errors
    /// Returns the daemon-advertised error category when the reply is
    /// [`Response::Error`].
    pub fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        match self.call(Request::CallTool {
            name: name.to_owned(),
            args,
        })? {
            Response::ToolResult { value } => Ok(value),
            Response::Error { code, message } => Err(Error::from_mcp_code(code, message)),
            other => Err(Error::Protocol {
                message: format!("unexpected tool reply: {other:?}"),
            }),
        }
    }
}

/// Path of the daemon socket inside the cache root (design §3).
#[must_use]
pub fn socket_path(cache: &Path) -> PathBuf {
    cache.join("state").join("daemon.sock")
}

fn io_error(err: std::io::Error) -> Error {
    Error::transport_with_source(err.to_string(), err)
}

/// True when the error came from the socket/framing rather than the daemon
/// answering with an error category; such connections must be reconnected.
#[must_use]
pub fn is_transport_error(err: &Error) -> bool {
    matches!(err, Error::Transport { .. })
}
