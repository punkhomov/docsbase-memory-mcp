# Daemon Protocol

## Transport

The daemon listens on a Unix socket (or a named pipe on Windows).
Every connection serves exactly one JSON request and one JSON response,
then closes. No keep-alive, no pipelining.

## Framing

Requests are length-prefixed JSON, and this exact detail broke two
client implementations before it was written down. The first client sent
newline-delimited JSON and worked until a request body contained an
embedded newline inside a string field; the server split the frame mid
request and answered with a parse error. The second client sent the raw
JSON with no prefix and relied on connection close to delimit the end;
that worked for one-shot calls but deadlocked the day the server started
accepting a second request per connection. A third attempt used HTTP
chunked encoding over the socket, which worked but dragged in a full HTTP
parser for a framing problem, plus chunk extensions that no client ever
sent; the dependency was removed after review as unjustified weight for
a local socket. A lighter proposal used a 4-byte length, and died on the
first payload above 4 GB in a stress test with a database snapshot; 8
bytes cost nothing and remove the ceiling forever. Varint encoding was
also discussed and dropped: variable-length prefixes complicate the
partial-read logic for zero benefit on a local socket. Text-based headers
were rejected for the same reason: debuggability does not justify a
parser where eight bytes suffice. The final contract, pinned by
a compatibility test: the client writes an 8-byte big-endian length, then
exactly that many bytes of UTF-8 JSON, then reads an 8-byte big-endian
length followed by the response bytes. The server never reads past the
declared length, so pipelined garbage after the frame is a protocol error,
not silently consumed input. Partial writes are retried until the full
frame is on the wire; a short read is a truncated frame and the connection
must be dropped, never retried blindly. A server that receives a length
larger than the 64 MB cap rejects the frame before allocating the buffer:
without the cap a single malicious length prefix turns into an OOM.
Every implementation must pass the
framing test with a 10 MB payload before it is allowed to merge.

## Request Schema

Every request carries `tool`, `params`, and `id`. Unknown tools get a
structured error with the tool name echoed, never a silent empty response.

## Response Envelope

Responses wrap the payload in `{ok, result | error}`. Transport errors
(connection refused, truncated frame) surface as client-side failures,
not as server error payloads.

## Timeouts

The client sets a 30-second deadline per call. The server does not enforce
call deadlines internally; a stuck call holds its connection until the
client gives up.

## Concurrency

The server handles connections on a thread pool. One slow client never
blocks others, but the pool is bounded: past 64 concurrent calls new
connections wait in the listen backlog.

## Versioning

The protocol version travels in the first frame header. Mismatched major
versions are rejected before any JSON is parsed.

## Shutdown

SIGTERM stops accepting new connections and drains in-flight calls up to
ten seconds before exiting. Clients see refused connections, not hangs.

## Errors and Retries

Only transport failures are retryable. A well-formed error response means
the server processed the call: retrying it may double-apply the effect.
