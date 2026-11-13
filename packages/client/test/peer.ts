import net, { type Socket } from 'node:net';
import { randomUUID } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import type { TestContext } from 'node:test';

export const hello = {
  kind: 'Hello',
  data: {
    protocol_version: 1,
    build_id: 'docsbase test',
    schema_version: 2,
    capabilities: ['search-context-v1', 'panel-read-v1', 'literal-query-v1'],
  },
};
export const ok = { kind: 'ToolResult', data: { value: { session_id: 1 } } };
export function line(value: unknown): string {
  return JSON.stringify(value) + '\n';
}
export async function peer(
  t: TestContext,
  handler: (request: Record<string, any>, socket: Socket) => void,
): Promise<string> {
  const endpoint =
    process.platform === 'win32'
      ? `\\\\.\\pipe\\docsbase-client-${randomUUID()}`
      : join(tmpdir(), `dbc-${randomUUID()}.sock`);
  const sockets = new Set<Socket>();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.on('error', () => {});
    socket.on('close', () => sockets.delete(socket));
    let pending = '';
    socket.on('data', (chunk) => {
      pending += chunk.toString();
      for (;;) {
        const end = pending.indexOf('\n');
        if (end < 0) break;
        const request = JSON.parse(pending.slice(0, end));
        pending = pending.slice(end + 1);
        handler(request, socket);
      }
    });
  });
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject);
    server.listen(endpoint, resolve);
  });
  t.after(async () => {
    for (const socket of sockets) socket.destroy();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  });
  return endpoint;
}
export function options(endpoint: string) {
  return {
    endpoint,
    projectId: 7,
    expectedBuildId: 'docsbase test',
    expectedSchemaVersion: 2,
    settings: {
      searchConnections: 2,
      controlConnections: 1,
      queueLimit: 2,
      requestTimeoutMs: 10000,
    },
  };
}
export function deferred<T = void>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
