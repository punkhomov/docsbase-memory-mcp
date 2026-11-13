import { createConnection, type Socket } from 'node:net';
import { abortError, DocsbaseError, type ClientOptions, type HelloMetadata } from './types.ts';
import {
  decodeResponse,
  encodeRequest,
  toolValue,
  MAX_FRAME_BYTES,
  PROTOCOL_VERSION,
  SEARCH_CONTEXT,
  PANEL_READ,
  type WireRequest,
  type WireResponse,
} from './protocol.ts';

/** The caller owns the original deadline and cancellation; transport never resets it. */
export interface OperationGuard {
  readonly signal: AbortSignal;
  check(): void;
}

type Pending = {
  resolve: (value: WireResponse) => void;
  reject: (error: DocsbaseError) => void;
  cleanup: () => void;
  buffer: Buffer;
  bytes: number;
  operation: OperationGuard;
};

/** Internal sequential channel. Interrupted/invalid exchanges always destroy it. */
export class Connection {
  readonly socket: Socket;
  metadata!: HelloMetadata;
  private pending: Pending | undefined;
  private closed = false;
  private constructor(socket: Socket) {
    this.socket = socket;
    socket.on('data', (bytes) => this.receive(bytes));
    socket.on('error', (cause) =>
      this.fail(new DocsbaseError('transport', 'docsbase socket failed', { cause })),
    );
    socket.on('end', () => this.fail(new DocsbaseError('transport', 'docsbase connection ended')));
    socket.on('close', () =>
      this.fail(new DocsbaseError('transport', 'docsbase connection closed')),
    );
  }
  get alive(): boolean {
    return !this.closed && !this.socket.destroyed;
  }

  static async open(
    options: Readonly<ClientOptions>,
    operation: OperationGuard,
  ): Promise<Connection> {
    operation.check();
    const { signal } = operation;
    if (signal.aborted) throw abortError(signal);
    const socket = createConnection({ path: options.endpoint });
    const connection = new Connection(socket);
    try {
      await new Promise<void>((resolve, reject) => {
        const cleanup = () => {
          socket.off('connect', connected);
          socket.off('error', failed);
          socket.off('close', closed);
          signal.removeEventListener('abort', aborted);
        };
        const connected = () => {
          cleanup();
          resolve();
        };
        const failed = (cause: Error) => {
          cleanup();
          reject(new DocsbaseError('transport', 'cannot connect to docsbase', { cause }));
        };
        const closed = () => {
          cleanup();
          reject(new DocsbaseError('transport', 'docsbase closed during connect'));
        };
        const aborted = () => {
          cleanup();
          connection.close();
          reject(abortError(signal));
        };
        socket.once('connect', connected);
        socket.once('error', failed);
        socket.once('close', closed);
        signal.addEventListener('abort', aborted, { once: true });
        if (signal.aborted) aborted();
      });
      const hello = await connection.exchange(
        {
          method: 'Hello',
          params: {
            protocol_version: PROTOCOL_VERSION,
            build_id: options.expectedBuildId,
            client: 'docsbase-node',
          },
        },
        operation,
      );
      connection.checkOperation(operation);
      if (hello.kind === 'Error') toolValue(hello);
      if (
        hello.kind !== 'Hello' ||
        hello.data.protocol_version !== PROTOCOL_VERSION ||
        hello.data.build_id !== options.expectedBuildId ||
        hello.data.schema_version !== options.expectedSchemaVersion ||
        ![SEARCH_CONTEXT, PANEL_READ].every((c) => hello.data.capabilities.includes(c))
      ) {
        throw new DocsbaseError(
          'protocol',
          'incompatible docsbase protocol, build, schema or capabilities',
        );
      }
      connection.metadata = Object.freeze({
        ...hello.data,
        capabilities: Object.freeze([...hello.data.capabilities]),
      });
      const bound = toolValue(
        await connection.exchange(
          {
            method: 'BindReadOnly',
            params: {
              pid: process.pid,
              project_id: options.projectId,
            },
          },
          operation,
        ),
      );
      if (
        !bound ||
        typeof bound !== 'object' ||
        !('session_id' in bound) ||
        !Number.isSafeInteger(bound.session_id) ||
        (bound.session_id as number) <= 0
      ) {
        throw new DocsbaseError('protocol', 'invalid docsbase session binding');
      }
      connection.checkOperation(operation);
      return connection;
    } catch (error) {
      connection.close();
      throw error;
    }
  }

  async exchange(request: WireRequest, operation: OperationGuard): Promise<WireResponse> {
    this.checkOperation(operation);
    const { signal } = operation;
    if (!this.alive || this.pending)
      throw new DocsbaseError('transport', 'docsbase connection unavailable');
    const bytes = encodeRequest(request);
    this.checkOperation(operation);
    return new Promise<WireResponse>((resolve, reject) => {
      const aborted = () => this.fail(abortError(signal));
      this.pending = {
        resolve,
        reject,
        buffer: Buffer.alloc(0),
        bytes: 0,
        operation,
        cleanup: () => signal.removeEventListener('abort', aborted),
      };
      signal.addEventListener('abort', aborted, { once: true });
      if (signal.aborted) {
        aborted();
        return;
      }
      this.checkOperation(operation);
      this.socket.write(bytes, (error) => {
        if (error)
          this.fail(new DocsbaseError('transport', 'docsbase write failed', { cause: error }));
      });
    });
  }

  close(): void {
    this.fail(new DocsbaseError('closed', 'docsbase connection closed'));
  }
  private checkOperation(operation: OperationGuard): void {
    try {
      operation.check();
      if (operation.signal.aborted) throw abortError(operation.signal);
    } catch (cause) {
      const error =
        cause instanceof DocsbaseError
          ? cause
          : new DocsbaseError('transport', 'docsbase operation check failed', { cause });
      this.fail(error);
      throw error;
    }
  }
  private fail(error: DocsbaseError): void {
    this.closed = true;
    const pending = this.pending;
    this.pending = undefined;
    if (pending) {
      pending.cleanup();
      pending.reject(error);
    }
    this.socket.destroy();
  }
  private receive(bytes: Buffer): void {
    const pending = this.pending;
    if (!pending || this.closed) {
      this.fail(new DocsbaseError('protocol', 'unsolicited docsbase response'));
      return;
    }
    const end = bytes.indexOf(10);
    const payload = end < 0 ? bytes : bytes.subarray(0, end);
    if (
      pending.bytes + payload.length > MAX_FRAME_BYTES ||
      (end >= 0 && end !== bytes.length - 1)
    ) {
      this.fail(new DocsbaseError('protocol', 'oversized or extra docsbase response'));
      return;
    }
    // One bounded buffer, rather than one retained allocation per fragment. A
    // peer sending byte-sized fragments cannot multiply Buffer/object overhead.
    const required = pending.bytes + payload.length;
    if (required > pending.buffer.length) {
      const next = Buffer.allocUnsafe(
        Math.min(MAX_FRAME_BYTES, Math.max(required, Math.max(1, pending.buffer.length) * 2)),
      );
      pending.buffer.copy(next, 0, 0, pending.bytes);
      pending.buffer = next;
    }
    payload.copy(pending.buffer, pending.bytes);
    pending.bytes = required;
    if (end < 0) return;
    try {
      const response = decodeResponse(pending.buffer.subarray(0, pending.bytes));
      this.checkOperation(pending.operation);
      this.pending = undefined;
      pending.cleanup();
      pending.resolve(response);
    } catch (error) {
      this.fail(
        error instanceof DocsbaseError
          ? error
          : new DocsbaseError('protocol', 'invalid docsbase response'),
      );
    }
  }
}
