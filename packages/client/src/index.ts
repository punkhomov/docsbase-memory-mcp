import { performance } from 'node:perf_hooks';
import { Connection } from './connection.ts';
import { Pool } from './pool.ts';
import { resolveClientOptions } from './settings.ts';
import {
  LITERAL_QUERY,
  MAX_FRAME_BYTES,
  encodeRequest,
  object,
  safeUnsigned,
  toolValue,
  searchResult,
  daemonStatus,
  type WireRequest,
} from './protocol.ts';
import {
  DocsbaseError,
  abortError,
  type ClientOptions,
  type CallOptions,
  type SearchOptions,
  type SearchResult,
  type DaemonStatus,
  type HelloMetadata,
} from './types.ts';
export { DocsbaseError } from './types.ts';
export type {
  ClientOptions,
  ClientSettings,
  CallOptions,
  SearchOptions,
  SearchHit,
  SearchResult,
  ProjectStatus,
  DaemonStatus,
  HelloMetadata,
  ErrorKind,
} from './types.ts';

/** One original operation. It never acquires an independent per-stage timeout. */
class Operation {
  readonly startedAt: number;
  readonly deadline: number;
  readonly controller = new AbortController();
  readonly signal = this.controller.signal;
  queueMs = 0;
  private readonly timer: ReturnType<typeof setTimeout>;
  private readonly upstream: AbortSignal | undefined;
  private readonly onAbort = () =>
    this.controller.abort(new DocsbaseError('cancelled', 'docsbase request cancelled'));
  constructor(startedAt: number, timeoutMs: number, upstream: AbortSignal | undefined) {
    this.startedAt = startedAt;
    this.deadline = startedAt + timeoutMs;
    this.upstream = upstream;
    if (upstream) {
      upstream.addEventListener('abort', this.onAbort, { once: true });
      if (upstream.aborted) this.onAbort();
    }
    this.timer = setTimeout(() => this.expire(), Math.max(1, this.deadline - performance.now()));
  }
  check(): void {
    if (performance.now() >= this.deadline && !this.signal.aborted) this.expire();
    if (this.signal.aborted) throw abortError(this.signal);
  }
  context(): { remaining_ms: number; elapsed_ms: number; queue_ms: number } {
    this.check();
    const now = performance.now();
    const remaining = Math.floor(this.deadline - now);
    if (remaining <= 0) {
      this.expire();
      throw abortError(this.signal);
    }
    return {
      remaining_ms: remaining,
      elapsed_ms: Math.floor(Math.max(0, now - this.startedAt)),
      queue_ms: this.queueMs,
    };
  }
  close(): void {
    this.controller.abort(new DocsbaseError('closed', 'docsbase client closed'));
  }
  finish(): void {
    clearTimeout(this.timer);
    this.upstream?.removeEventListener('abort', this.onAbort);
  }
  private expire(): void {
    this.controller.abort(new DocsbaseError('deadline', 'docsbase request deadline expired'));
  }
}

const searchFields = new Set([
  'query_mode',
  'limit',
  'explain',
  'snippet',
  'rerank',
  'dedupe',
  'n_per_doc',
  'path_prefix',
  'tags',
]);
function prepareSearch(query: string, options: SearchOptions): Record<string, unknown> {
  if (typeof query !== 'string' || Buffer.byteLength(query) > MAX_FRAME_BYTES || !object(options)) {
    throw new DocsbaseError('validation', 'invalid docsbase search arguments');
  }
  const args: Record<string, unknown> = { query };
  let bytes = Buffer.byteLength(query);
  for (const key of Object.keys(options)) {
    if (!searchFields.has(key))
      throw new DocsbaseError('validation', 'unknown docsbase search option');
    const value: unknown = (options as Record<string, unknown>)[key];
    if (value === undefined) continue;
    let valid = value === null;
    if (key === 'query_mode') valid ||= value === 'syntax' || value === 'literal';
    else if (key === 'dedupe') valid ||= value === 'none' || value === 'path';
    else if (key === 'limit' || key === 'n_per_doc') valid ||= safeUnsigned(value);
    else if (['explain', 'snippet', 'rerank'].includes(key)) valid ||= typeof value === 'boolean';
    else if (key === 'path_prefix') valid ||= typeof value === 'string';
    else if (key === 'tags') valid ||= Array.isArray(value);
    if (!valid) throw new DocsbaseError('validation', 'invalid docsbase search option');
    if (typeof value === 'string') bytes += Buffer.byteLength(value);
    if (Array.isArray(value)) {
      const tags: string[] = [];
      for (const tag of value) {
        if (typeof tag !== 'string')
          throw new DocsbaseError('validation', 'invalid docsbase search option');
        bytes += Buffer.byteLength(tag) + 3;
        if (bytes > MAX_FRAME_BYTES)
          throw new DocsbaseError('validation', 'docsbase request exceeds IPC frame limit');
        tags.push(tag);
      }
      args[key] = tags;
    } else {
      args[key] = value;
    }
    if (bytes > MAX_FRAME_BYTES)
      throw new DocsbaseError('validation', 'docsbase request exceeds IPC frame limit');
  }
  // Check the complete wire envelope with maximum representable context fields
  // before retaining queued work; no network side effect for oversize arguments.
  encodeRequest({
    method: 'Search',
    params: {
      args,
      context: {
        remaining_ms: Number.MAX_SAFE_INTEGER,
        elapsed_ms: Number.MAX_SAFE_INTEGER,
        queue_ms: Number.MAX_SAFE_INTEGER,
      },
    },
  });
  return args;
}

/** Native local IPC only. Construction performs no I/O or administrative work. */
export class DocsbaseClient {
  private readonly options: Readonly<ClientOptions>;
  private readonly searches: Pool;
  private readonly controls: Pool;
  private readonly operations = new Set<Operation>();
  private closed = false;
  private lastHello: HelloMetadata | undefined;
  constructor(options: ClientOptions) {
    this.options = resolveClientOptions(options);
    this.searches = new Pool(
      this.options.settings.searchConnections,
      this.options.settings.queueLimit,
    );
    this.controls = new Pool(
      this.options.settings.controlConnections,
      this.options.settings.queueLimit,
    );
  }
  /** Diagnostics from the last successful handshake; not a fresh readiness proof. */
  get metadata(): HelloMetadata | undefined {
    return this.lastHello;
  }

  async search(
    query: string,
    options: SearchOptions = {},
    call: CallOptions = {},
  ): Promise<SearchResult> {
    const startedAt = performance.now();
    const operation = this.begin(startedAt, call);
    try {
      const args = prepareSearch(query, options);
      operation.check();
      return await this.invoke(
        this.searches,
        operation,
        (connection) => {
          if (
            args.query_mode === 'literal' &&
            !connection.metadata.capabilities.includes(LITERAL_QUERY)
          ) {
            throw new DocsbaseError('protocol', 'daemon does not support literal-query-v1');
          }
          return { method: 'Search', params: { args, context: operation.context() } };
        },
        searchResult,
      );
    } finally {
      this.end(operation);
    }
  }
  /** Registry diagnostics for the immutable bound project; not a publication fence. */
  async status(call: CallOptions = {}): Promise<DaemonStatus> {
    const startedAt = performance.now();
    const operation = this.begin(startedAt, call);
    try {
      operation.check();
      return await this.invoke(
        this.controls,
        operation,
        () => ({ method: 'CallTool', params: { name: 'status', args: {} } }),
        (value) => daemonStatus(value, this.options.projectId),
      );
    } finally {
      this.end(operation);
    }
  }
  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const operation of this.operations) operation.close();
    this.searches.close();
    this.controls.close();
  }
  private begin(startedAt: number, call: CallOptions): Operation {
    if (this.closed) throw new DocsbaseError('closed', 'docsbase client closed');
    if (!object(call) || Object.keys(call).some((key) => key !== 'signal')) {
      throw new DocsbaseError('validation', 'invalid docsbase call options');
    }
    const signal = call.signal;
    if (signal !== undefined && !(signal instanceof AbortSignal)) {
      throw new DocsbaseError('validation', 'invalid docsbase AbortSignal');
    }
    const operation = new Operation(startedAt, this.options.settings.requestTimeoutMs, signal);
    try {
      operation.check();
    } catch (error) {
      operation.finish();
      throw error;
    }
    this.operations.add(operation);
    return operation;
  }
  private end(operation: Operation): void {
    operation.finish();
    this.operations.delete(operation);
  }
  private async invoke<T>(
    pool: Pool,
    operation: Operation,
    request: (connection: Connection) => WireRequest,
    decode: (value: unknown) => T,
  ): Promise<T> {
    const lease = await pool.acquire(operation.signal);
    operation.queueMs = lease.queueMs;
    let reusable = false;
    try {
      operation.check();
      const connection = lease.slot.connection ?? (await Connection.open(this.options, operation));
      lease.slot.connection = connection;
      this.lastHello = connection.metadata;
      operation.check();
      const response = await connection.exchange(request(connection), operation);
      operation.check();
      const result = decode(toolValue(response));
      operation.check();
      reusable = true;
      return result;
    } catch (error) {
      reusable = error instanceof DocsbaseError && error.kind === 'daemon';
      throw error;
    } finally {
      if (!reusable) lease.slot.connection?.close();
      pool.release(lease);
    }
  }
}
