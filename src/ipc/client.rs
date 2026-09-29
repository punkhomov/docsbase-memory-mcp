//! Socket-first CLI client (FR-30, design §6).

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::ipc::protocol::{self, PROTOCOL_VERSION, Request, Response};
use crate::platform::{self, BlockingStream};

/// Timeout for a single request/response exchange.
const IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Connected daemon client.
pub struct Client {
    stream: BufReader<BlockingStream>,
}

impl Client {
    /// Connects to the daemon endpoint under `cache`.
    ///
    /// Returns `None` when no daemon is listening (missing or stale socket),
    /// which callers treat as "fall back to direct mode".
    #[must_use]
    pub fn connect(cache: &Path) -> Option<Self> {
        // No existence pre-check: on Windows a named pipe has no file to
        // stat, and a failed connect already means "no daemon".
        let endpoint = platform::daemon_endpoint(cache);
        let stream = platform::connect_blocking(&endpoint).ok()?;
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
            Response::Error {
                code,
                message,
                instruction,
            } => Err(Error::from_mcp_code(code, message, instruction)),
            other @ Response::ToolResult { .. } => Err(Error::Protocol {
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
            Response::Error {
                code,
                message,
                instruction,
            } => Err(Error::from_mcp_code(code, message, instruction)),
            other @ Response::Hello { .. } => Err(Error::Protocol {
                message: format!("unexpected session reply: {other:?}"),
            }),
        }
    }

    /// Registers a cwd-only session (no project binding) after a failed
    /// [`Self::handshake`]; `index_project` can then default to the cwd (C8).
    ///
    /// # Errors
    /// Returns protocol/admission errors on version mismatch; a failed
    /// registration is reported as a daemon error.
    pub fn register_unbound(&mut self, cwd: &Path) -> Result<()> {
        match self.call(Request::RegisterUnbound {
            pid: std::process::id(),
            cwd: cwd.to_path_buf(),
        })? {
            Response::ToolResult { .. } => Ok(()),
            Response::Error {
                code,
                message,
                instruction,
            } => Err(Error::from_mcp_code(code, message, instruction)),
            other @ Response::Hello { .. } => Err(Error::Protocol {
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
            Response::Error {
                code,
                message,
                instruction,
            } => Err(Error::from_mcp_code(code, message, instruction)),
            other @ Response::Hello { .. } => Err(Error::Protocol {
                message: format!("unexpected tool reply: {other:?}"),
            }),
        }
    }
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
