//! Asynchronous local IPC with explicit scoped-search deadlines.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader, ReadHalf, WriteHalf};

use crate::error::{Error, Result};
use crate::hybrid::work::Cancellation;
use crate::platform::{self, Stream};
use crate::search::operation::SearchOperation;

use super::framing::{Frame, FrameReader};
use super::protocol::{self, Request, Response, SEARCH_CONTEXT_CAPABILITY, SearchCallContext};

/// One serialized connection; an interrupted exchange must never be reused.
/// Generic exchanges are bounded by their composition owner, not a local default.
pub struct AsyncClient {
    reader: FrameReader<BufReader<ReadHalf<Stream>>>,
    writer: WriteHalf<Stream>,
    capabilities: Vec<String>,
    poisoned: bool,
}

impl AsyncClient {
    /// Opens the native endpoint asynchronously. The caller owns admission time.
    ///
    /// # Errors
    /// Returns the platform connection error when no endpoint can be opened.
    pub async fn connect(cache: &Path) -> std::io::Result<Self> {
        let stream = platform::connect_async(&platform::daemon_endpoint(cache)).await?;
        let (read, writer) = tokio::io::split(stream);
        Ok(Self {
            reader: FrameReader::new(BufReader::new(read)),
            writer,
            capabilities: Vec::new(),
            poisoned: false,
        })
    }

    /// Exchanges one capped frame. Dropping this future poisons the connection.
    ///
    /// # Errors
    /// Returns Transport on IO, framing, malformed responses or prior poisoning.
    /// Returns Internal if request encoding fails.
    pub async fn call(&mut self, request: Request) -> Result<Response> {
        if self.poisoned {
            return Err(Error::transport("client connection is poisoned; reconnect"));
        }
        let bytes = protocol::encode(&request)?;
        // Remains true on error or future drop, including a partially read frame.
        self.poisoned = true;
        self.writer.write_all(&bytes).await.map_err(io_error)?;
        let response = match self.reader.read_frame().await.map_err(io_error)? {
            Frame::Line(line) => protocol::decode_response(&line)
                .map_err(|error| Error::transport(format!("bad response: {error}")))?,
            Frame::Eof => return Err(Error::transport("connection closed")),
            Frame::Incomplete => return Err(Error::transport("incomplete response frame")),
            Frame::TooLarge => {
                return Err(Error::transport(format!(
                    "response frame exceeds {} bytes",
                    protocol::MAX_FRAME_BYTES,
                )));
            }
        };
        self.poisoned = false;
        Ok(response)
    }

    /// Negotiates protocol/capabilities without binding a project.
    ///
    /// # Errors
    /// Returns transport/protocol errors or the daemon's advertised category.
    pub async fn handshake_registry(&mut self) -> Result<()> {
        match self
            .call(Request::Hello {
                protocol_version: protocol::PROTOCOL_VERSION,
                build_id: protocol::build_id(),
                client: "docsbase-async".to_owned(),
            })
            .await?
        {
            Response::Hello {
                protocol_version,
                capabilities,
                ..
            } => {
                protocol::ensure_protocol_version(protocol_version)?;
                self.capabilities = capabilities;
                Ok(())
            }
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

    /// Negotiates and registers a cwd-bound session.
    ///
    /// # Errors
    /// Returns transport/protocol errors or the daemon's advertised category.
    pub async fn handshake(&mut self, cwd: &Path) -> Result<()> {
        self.handshake_registry().await?;
        value(
            self.call(Request::RegisterSession {
                pid: std::process::id(),
                cwd: cwd.to_path_buf(),
            })
            .await?,
        )
        .map(|_| ())
    }

    /// Registers an unbound session after an unsuccessful project bind.
    ///
    /// # Errors
    /// Returns transport/protocol errors or the daemon's advertised category.
    pub async fn register_unbound(&mut self, cwd: &Path) -> Result<()> {
        value(
            self.call(Request::RegisterUnbound {
                pid: std::process::id(),
                cwd: cwd.to_path_buf(),
            })
            .await?,
        )
        .map(|_| ())
    }

    /// Calls a generic tool under the caller's externally applied time budget.
    /// Scoped search callers use [`Self::search`] instead.
    ///
    /// # Errors
    /// Returns transport/protocol errors or the daemon's advertised category.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        value(
            self.call(Request::CallTool {
                name: name.to_owned(),
                args,
            })
            .await?,
        )
    }

    /// Sends scoped search only after capability discovery, using one operation.
    ///
    /// # Errors
    /// Admission on missing capability, cancellation or expiry; otherwise
    /// transport/protocol errors or the daemon's original error category.
    pub async fn search(&mut self, args: Value, operation: &SearchOperation) -> Result<Value> {
        within_operation(operation, self.exchange_search(args, operation)).await
    }

    #[cfg(test)]
    pub(super) async fn search_before(
        &mut self,
        args: Value,
        operation: &SearchOperation,
        expiry: impl Future<Output = ()>,
    ) -> Result<Value> {
        within_before(operation, self.exchange_search(args, operation), expiry).await
    }

    async fn exchange_search(&mut self, args: Value, operation: &SearchOperation) -> Result<Value> {
        operation.check()?;
        if !self
            .capabilities
            .iter()
            .any(|capability| capability == SEARCH_CONTEXT_CAPABILITY)
        {
            return Err(Error::Admission {
                message: format!(
                    "daemon does not advertise {SEARCH_CONTEXT_CAPABILITY}; restart the daemon and frontend"
                ),
            });
        }
        let explain = args.get("explain").and_then(Value::as_bool) == Some(true);
        let context = SearchCallContext {
            elapsed_ms: milliseconds(operation.elapsed()),
            remaining_ms: milliseconds(operation.remaining()?),
            queue_ms: operation.queue_ms(),
        };
        context.validate()?;
        let response = self.call(Request::Search { args, context }).await?;
        let mut result = value(response)?;
        if explain
            && let Some(total) = result
                .get_mut("meta")
                .and_then(|meta| meta.get_mut("stage_ms"))
                .and_then(|stages| stages.get_mut("total"))
        {
            *total = json!(milliseconds(operation.elapsed()));
        }
        Ok(result)
    }
}

/// Applies the original operation across caller composition, including admission.
pub(crate) async fn within_operation<T>(
    operation: &SearchOperation,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    within_before(
        operation,
        work,
        tokio::time::sleep_until(tokio::time::Instant::from_std(operation.deadline())),
    )
    .await
}

async fn within_before<T>(
    operation: &SearchOperation,
    work: impl Future<Output = Result<T>>,
    expiry: impl Future<Output = ()>,
) -> Result<T> {
    operation.check()?;
    let mut cancellation = CancelOnDrop(Some(operation.cancellation().clone()));
    tokio::select! {
        biased;
        () = expiry => {
            operation.cancellation().cancel();
            Err(Error::Admission { message: "search request deadline expired".to_owned() })
        }
        result = work => {
            cancellation.0 = None;
            operation.check()?;
            result
        }
    }
}

struct CancelOnDrop(Option<Cancellation>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(token) = &self.0 {
            token.cancel();
        }
    }
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn io_error(error: std::io::Error) -> Error {
    Error::transport_with_source("async daemon IO", error)
}

fn value(response: Response) -> Result<Value> {
    match response {
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
