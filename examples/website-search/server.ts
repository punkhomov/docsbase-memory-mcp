/** Bounded HTTP embedding of the official read-only SDK. */
import http, { type IncomingMessage, type ServerResponse } from 'node:http';
import { performance } from 'node:perf_hooks';
import { once } from 'node:events';
import { DocsbaseError, type DocsbaseClient, type SearchOptions } from '@docsbase/client';
import { resolveWebsiteConfig, type WebsiteConfig } from './config.ts';

export type SearchClient = Pick<DocsbaseClient, 'search' | 'serverStatus' | 'close'>;
class HttpError extends Error {
  readonly status: number;
  readonly code: string;
  constructor(status: number, code: string) {
    super(code);
    this.status = status;
    this.code = code;
  }
}
class RequestOperation {
  private readonly req: IncomingMessage;
  private readonly res: ServerResponse;
  readonly controller = new AbortController();
  readonly signal = this.controller.signal;
  readonly budget: {
    readonly startedAtMs: number;
    readonly deadlineAtMs: number;
  };
  private readonly timer: ReturnType<typeof setTimeout>;
  private readonly disconnect = () =>
    this.controller.abort(new DocsbaseError('cancelled', 'HTTP caller disconnected'));
  private readonly responseClose = () => {
    if (!this.res.writableEnded) this.disconnect();
  };
  private readonly requestClose = () => {
    // IncomingMessage.close also follows a successfully consumed body.
    this.req.off('aborted', this.disconnect);
    this.req.off('error', this.disconnect);
  };
  constructor(req: IncomingMessage, res: ServerResponse, startedAtMs: number, timeout: number) {
    this.req = req;
    this.res = res;
    this.budget = Object.freeze({
      startedAtMs,
      deadlineAtMs: startedAtMs + timeout,
    });
    req.once('aborted', this.disconnect);
    req.once('error', this.disconnect);
    req.once('close', this.requestClose);
    res.once('close', this.responseClose);
    res.once('error', this.disconnect);
    this.timer = setTimeout(
      () => this.expire(),
      Math.max(1, this.budget.deadlineAtMs - performance.now()),
    );
    if (req.aborted || res.destroyed) this.disconnect();
  }
  check(): void {
    if (performance.now() >= this.budget.deadlineAtMs) this.expire();
    if (this.signal.aborted) throw this.signal.reason;
  }
  private expire(): void {
    this.controller.abort(new DocsbaseError('deadline', 'HTTP request deadline expired'));
  }
  finish(): void {
    clearTimeout(this.timer);
    this.req.off('aborted', this.disconnect);
    this.req.off('error', this.disconnect);
    this.req.off('close', this.requestClose);
    this.res.off('close', this.responseClose);
    this.res.off('error', this.disconnect);
  }
}
type Waiter = {
  operation: RequestOperation;
  resolve: (release: () => void) => void;
  reject: (error: unknown) => void;
  abort: () => void;
};
class Lane {
  private active = 0;
  private readonly pending: Waiter[] = [];
  private readonly capacity: number;
  private readonly queueLimit: number;
  constructor(capacity: number, queueLimit: number) {
    this.capacity = capacity;
    this.queueLimit = queueLimit;
  }
  acquire(operation: RequestOperation): Promise<() => void> {
    operation.check();
    if (this.active < this.capacity && this.pending.length === 0) {
      this.active++;
      return Promise.resolve(this.release());
    }
    if (this.pending.length >= this.queueLimit)
      throw new DocsbaseError('overloaded', 'HTTP admission full');
    return new Promise((resolve, reject) => {
      const waiter: Waiter = {
        operation,
        resolve,
        reject,
        abort: () => {
          const index = this.pending.indexOf(waiter);
          if (index !== -1) this.pending.splice(index, 1);
          operation.signal.removeEventListener('abort', waiter.abort);
          reject(operation.signal.reason);
        },
      };
      operation.signal.addEventListener('abort', waiter.abort, { once: true });
      this.pending.push(waiter);
      if (operation.signal.aborted) waiter.abort();
    });
  }
  private release(): () => void {
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.active--;
      while (this.pending.length && this.active < this.capacity) {
        const waiter = this.pending.shift()!;
        waiter.operation.signal.removeEventListener('abort', waiter.abort);
        try {
          waiter.operation.check();
          this.active++;
          waiter.resolve(this.release());
        } catch (error) {
          waiter.reject(error);
        }
      }
    };
  }
}
function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function body(req: IncomingMessage, limit: number, operation: RequestOperation): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    let bytes = 0;
    const chunks: Buffer[] = [];
    const cleanup = () => {
      req.off('data', data);
      req.off('end', end);
      req.off('error', error);
      operation.signal.removeEventListener('abort', abort);
    };
    const error = (error: unknown) => {
      cleanup();
      req.pause();
      reject(error);
    };
    const data = (chunk: Buffer) => {
      bytes += chunk.length;
      if (bytes > limit) error(new HttpError(413, 'body_too_large'));
      else chunks.push(chunk);
    };
    const end = () => {
      cleanup();
      resolve(Buffer.concat(chunks, bytes));
    };
    const abort = () => error(operation.signal.reason);
    req.on('data', data);
    req.once('end', end);
    req.once('error', error);
    operation.signal.addEventListener('abort', abort, { once: true });
    if (operation.signal.aborted) abort();
    else req.resume();
  });
}
function searchArguments(
  bytes: Buffer,
  maxQueryBytes: number,
  maxHits: number,
): { query: string; options: SearchOptions } {
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
  } catch {
    throw new HttpError(400, 'invalid_json');
  }
  if (
    !object(value) ||
    Object.keys(value).some((key) => key !== 'query' && key !== 'options') ||
    typeof value.query !== 'string' ||
    !value.query.trim() ||
    Buffer.byteLength(value.query) > maxQueryBytes ||
    (value.options !== undefined && !object(value.options))
  )
    throw new HttpError(400, 'invalid_search');
  const options: Record<string, unknown> = { limit: maxHits };
  for (const [key, entry] of Object.entries(value.options ?? {})) {
    let valid = false;
    if (key === 'query_mode') valid = entry === 'literal' || entry === 'syntax';
    if (key === 'limit')
      valid = Number.isSafeInteger(entry) && (entry as number) >= 1 && (entry as number) <= maxHits;
    if (key === 'snippet' || key === 'explain') valid = typeof entry === 'boolean';
    if (key === 'dedupe') valid = entry === 'none' || entry === 'path';
    // n_per_doc's maximum10 is the daemon search contract, not a default.
    if (key === 'n_per_doc')
      valid = Number.isSafeInteger(entry) && (entry as number) >= 1 && (entry as number) <= 10;
    if (key === 'path_prefix') valid = typeof entry === 'string';
    if (key === 'tags')
      valid = Array.isArray(entry) && entry.every((tag) => typeof tag === 'string');
    if (!valid) throw new HttpError(400, 'invalid_search_options');
    options[key] = entry;
  }
  return { query: value.query, options: options as SearchOptions };
}
function publicError(error: unknown): HttpError {
  if (error instanceof HttpError) return error;
  if (error instanceof DocsbaseError) {
    if (error.kind === 'deadline') return new HttpError(504, 'deadline');
    if (error.kind === 'validation' || (error.kind === 'daemon' && error.code === -32014))
      return new HttpError(400, 'invalid_search');
    if (
      ['overloaded', 'transport', 'closed', 'cancelled'].includes(error.kind) ||
      (error.kind === 'daemon' && [-32010, -32012].includes(error.code!))
    )
      return new HttpError(503, 'unavailable');
  }
  return new HttpError(502, 'upstream_error');
}
function send(
  res: ServerResponse,
  status: number,
  json: string,
  operation: RequestOperation,
): Promise<void> {
  operation.check();
  return new Promise((resolve, reject) => {
    const abort = () => {
      cleanup();
      res.destroy();
      reject(operation.signal.reason);
    };
    const error = (error: Error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      operation.signal.removeEventListener('abort', abort);
      res.off('error', error);
    };
    operation.signal.addEventListener('abort', abort, { once: true });
    res.once('error', error);
    res.writeHead(status, {
      'content-type': 'application/json; charset=utf-8',
      'content-length': Buffer.byteLength(json),
    });
    res.end(json, () => {
      cleanup();
      resolve();
    });
  });
}
export function createWebsiteServer(config: WebsiteConfig, api: SearchClient = config.client) {
  const policy = config.http;
  const search = new Lane(policy.searchActiveRequests, policy.searchPendingRequests);
  const control = new Lane(policy.controlActiveRequests, policy.controlPendingRequests);
  const operations = new Set<RequestOperation>();
  const tasks = new Set<Promise<void>>();
  let closing = false;
  let closed: Promise<void> | undefined;
  const server = http.createServer(
    {
      maxHeaderSize: policy.maxHeaderBytes,
      highWaterMark: policy.maxBodyBytes,
      headersTimeout: policy.headersTimeoutMs,
      requestTimeout: policy.requestTimeoutMs,
      connectionsCheckingInterval: policy.headerCheckIntervalMs,
      keepAliveTimeout: policy.keepAliveTimeoutMs,
      keepAliveTimeoutBuffer: 0,
      insecureHTTPParser: false,
      requireHostHeader: true,
    },
    (req, res) => {
      const startedAt = performance.now(); // First application action, before body/admission.
      const task = handle(req, res, startedAt);
      tasks.add(task);
      void task.finally(() => tasks.delete(task));
    },
  );
  server.maxConnections = policy.maxConnections;
  server.maxHeadersCount = policy.maxHeadersCount + 1;
  server.maxRequestsPerSocket = policy.maxRequestsPerSocket;
  server.setTimeout(policy.idleTimeoutMs, (socket) => socket.destroy());
  server.on('clientError', (error, socket) => {
    if (socket.writable)
      socket.end(
        `HTTP/1.1 ${'code' in error && error.code === 'HPE_HEADER_OVERFLOW' ? '431 Request Header Fields Too Large' : '400 Bad Request'}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n`,
      );
    else socket.destroy();
  });
  async function handle(
    req: IncomingMessage,
    res: ServerResponse,
    startedAt: number,
  ): Promise<void> {
    const operation = new RequestOperation(req, res, startedAt, policy.requestTimeoutMs);
    operations.add(operation);
    let release: (() => void) | undefined;
    req.pause();
    try {
      operation.check();
      if (closing) throw new HttpError(503, 'unavailable');
      if (req.rawHeaders.length / 2 > policy.maxHeadersCount)
        throw new HttpError(431, 'headers_too_large');
      const ready = req.url === '/ready';
      if (!ready && req.url !== '/search') throw new HttpError(404, 'not_found');
      if (req.method !== (ready ? 'GET' : 'POST')) {
        res.setHeader('allow', ready ? 'GET' : 'POST');
        throw new HttpError(405, 'method_not_allowed');
      }
      if (
        ready &&
        (req.headers['transfer-encoding'] ||
          (req.headers['content-length'] && req.headers['content-length'] !== '0'))
      )
        throw new HttpError(400, 'unexpected_body');
      if (
        !ready &&
        req.headers['content-type']?.split(';')[0]?.trim().toLowerCase() !== 'application/json'
      )
        throw new HttpError(415, 'json_required');
      if (
        req.headers['content-length'] &&
        Number(req.headers['content-length']) > policy.maxBodyBytes
      )
        throw new HttpError(413, 'body_too_large');
      release = await (ready ? control : search).acquire(operation);
      operation.check();
      const call = { signal: operation.signal, budget: operation.budget };
      let value: unknown;
      let status = 200;
      if (ready) {
        const result = await api.serverStatus(call);
        value = {
          ready: result.ready,
          reason: result.reason,
          index_generation: result.index_generation,
          restart_required: result.restart_required,
        };
        if (!result.ready) status = 503;
      } else {
        const bytes = await body(req, policy.maxBodyBytes, operation);
        operation.check();
        const args = searchArguments(bytes, policy.maxQueryBytes, policy.maxHits);
        operation.check();
        value = await api.search(args.query, args.options, call);
      }
      operation.check();
      const json = JSON.stringify(value);
      operation.check();
      if (Buffer.byteLength(json) > policy.maxResultBytes)
        throw new HttpError(502, 'result_too_large');
      await send(res, status, json, operation);
    } catch (error) {
      if (!res.destroyed && !res.writableEnded) {
        if (res.headersSent) res.destroy();
        else {
          const failure = publicError(error);
          const json = JSON.stringify({ error: failure.code });
          res.shouldKeepAlive = false;
          res.writeHead(failure.status, {
            'content-type': 'application/json; charset=utf-8',
            connection: 'close',
          });
          res.end(Buffer.byteLength(json) <= policy.maxResultBytes ? json : undefined);
        }
      }
    } finally {
      operation.finish();
      operations.delete(operation);
      release?.();
    }
  }
  function close(): Promise<void> {
    if (closed) return closed;
    closing = true;
    for (const operation of operations)
      operation.controller.abort(new DocsbaseError('closed', 'HTTP server closed'));
    api.close();
    if (api !== config.client) config.client.close();
    closed = new Promise<void>((resolve) => {
      server.close(() => resolve());
      server.closeAllConnections();
    }).then(async () => {
      await Promise.allSettled([...tasks]);
    });
    return closed;
  }
  return { server, close };
}

if (import.meta.main) {
  try {
    const raw = process.env.DOCSBASE_WEBSITE_CONFIG;
    if (raw === undefined)
      throw new Error('DOCSBASE_WEBSITE_CONFIG must contain the complete operator JSON');
    const config = resolveWebsiteConfig(JSON.parse(raw));
    const service = createWebsiteServer(config);
    service.server.listen(config.http.port, config.http.host);
    await once(service.server, 'listening');
    process.stdout.write(JSON.stringify({ listening: service.server.address() }) + '\n');
    for (const signal of ['SIGINT', 'SIGTERM'] as const)
      process.once(signal, () => {
        void service.close();
      });
  } catch {
    process.stderr.write('website startup failed: invalid policy or unavailable listener\n');
    process.exitCode = 1;
  }
}
