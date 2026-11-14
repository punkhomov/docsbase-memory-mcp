import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import net from 'node:net';
import { once } from 'node:events';
import { performance } from 'node:perf_hooks';
import { DocsbaseError } from '@docsbase/client';
import { peer, hello, ok, line } from '../../../packages/client/test/peer.ts';
import { resolveWebsiteConfig } from '../config.ts';
import { createWebsiteServer, type SearchClient } from '../server.ts';

function deferred<T = void>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
function input() {
  return {
    http: {
      host: '127.0.0.1',
      port: 0,
      maxConnections: 16,
      searchActiveRequests: 2,
      searchPendingRequests: 0,
      controlActiveRequests: 1,
      controlPendingRequests: 0,
      maxHeaderBytes: 4096,
      maxHeadersCount: 16,
      maxBodyBytes: 512,
      maxQueryBytes: 128,
      maxResultBytes: 4096,
      maxHits: 10,
      requestTimeoutMs: 10000,
      headersTimeoutMs: 10000,
      headerCheckIntervalMs: 1000,
      idleTimeoutMs: 10000,
      keepAliveTimeoutMs: 1000,
      maxRequestsPerSocket: 20,
    },
    client: {
      endpoint:
        process.platform === 'win32'
          ? String.raw`\\.\pipe\unused-response-test`
          : '/unused-response-test',
      projectId: 7,
      expectedBuildId: 'docsbase test',
      expectedSchemaVersion: 2,
      settings: {
        searchConnections: 1,
        controlConnections: 1,
        queueLimit: 0,
        requestTimeoutMs: 10000,
      },
    },
  };
}
async function listen(t: TestContext, api?: SearchClient, settings = input()) {
  const config = resolveWebsiteConfig(settings);
  const service = api ? createWebsiteServer(config, api) : createWebsiteServer(config);
  service.server.listen(0, '127.0.0.1');
  await once(service.server, 'listening');
  const address = service.server.address();
  assert.ok(address && typeof address === 'object');
  t.after(() => service.close());
  return { service, port: address.port };
}
function request(port: number, query = 'replacement', path = '/search', marker = '') {
  let req!: http.ClientRequest;
  const result = new Promise<{ status: number; body: string }>((resolve, reject) => {
    req = http.request(
      {
        host: '127.0.0.1',
        port,
        path,
        method: path === '/ready' ? 'GET' : 'POST',
        agent: false,
        headers: { 'content-type': 'application/json', 'x-case': marker },
      },
      (res) => {
        let text = '';
        res.setEncoding('utf8');
        res.on('data', (chunk) => {
          text += chunk;
        });
        res.on('error', reject);
        res.on('end', () => resolve({ status: res.statusCode!, body: text }));
      },
    );
    req.on('error', reject);
    req.end(path === '/ready' ? undefined : JSON.stringify({ query }));
  });
  void result.catch(() => {});
  return { req, result };
}
for (const route of ['/search', '/ready']) {
  test(`HTTP deadline during active official SDK ${route} is 504 without replay`, {
    timeout: 60000,
  }, async (t) => {
    let now = 0;
    let calls = 0;
    const arrived = deferred();
    const disconnected = deferred();
    t.mock.method(performance, 'now', () => now);
    t.mock.timers.enable({ apis: ['setTimeout'] });
    const endpoint = await peer(t, (message, socket) => {
      if (message.method === 'Hello')
        socket.write(
          line({
            ...hello,
            data: { ...hello.data, capabilities: [...hello.data.capabilities, 'server-status-v1'] },
          }),
        );
      else if (message.method === 'BindReadOnly') socket.write(line(ok));
      else {
        calls++;
        socket.once('close', () => disconnected.resolve());
        arrived.resolve();
      }
    });
    const settings = input();
    settings.client.endpoint = endpoint;
    const { port } = await listen(t, undefined, settings);
    const pending = request(port, 'deadline', route);
    await arrived.promise;
    now = 10000;
    t.mock.timers.tick(10000);
    const response = await pending.result;
    assert.equal(response.status, 504);
    assert.deepEqual(JSON.parse(response.body), { error: 'deadline' });
    await disconnected.promise;
    assert.equal(calls, 1);
  });
}
function frame(query: string, marker: string) {
  const body = JSON.stringify({ query });
  return `POST /search HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: ${Buffer.byteLength(body)}\r\nX-Case: ${marker}\r\n\r\n${body}`;
}
async function pipeline(t: TestContext, failure: boolean) {
  const release = deferred<[]>();
  const ended = deferred<http.ServerResponse>();
  const consumed = deferred();
  const signals = new Map<string, AbortSignal>();
  const seen: string[] = [];
  const original = http.ServerResponse.prototype.end;
  t.mock.method(
    http.ServerResponse.prototype,
    'end',
    function (this: http.ServerResponse, ...args: any[]) {
      const value = Reflect.apply(original, this, args);
      if (this.req.headers['x-case'] === 'queued') ended.resolve(this);
      return value;
    },
  );
  const api: SearchClient = {
    search: async (query, _options, call) => {
      signals.set(query, call!.signal!);
      seen.push(query);
      if (query === 'first') return release.promise;
      if (query === 'queued' && failure) throw new DocsbaseError('transport', 'private failure');
      return [];
    },
    serverStatus: async () => {
      throw new Error('unused');
    },
    close() {},
  };
  const { port, service } = await listen(t, api);
  service.server.on('request', (req) => {
    if (req.headers['x-case'] === 'queued') req.once('close', () => consumed.resolve());
  });
  const socket = net.connect({ host: '127.0.0.1', port });
  socket.on('error', () => {});
  socket.on('data', () => {});
  await once(socket, 'connect');
  socket.write(frame('first', 'first') + frame('queued', 'queued'));
  const response = await ended.promise;
  await consumed.promise;
  assert.equal(response.writableEnded, true);
  assert.equal(response.writableFinished, false);
  return { release, socket, response, signals, seen, port };
}

test('pipelined completed POST close after end cancels queued response and promptly frees its lane', {
  timeout: 60000,
}, async (t) => {
  const state = await pipeline(t, false);
  try {
    const closed = new Promise<void>((resolve) => state.response.once('close', resolve));
    state.socket.destroy();
    await closed;
    assert.equal(state.signals.get('queued')!.aborted, true);
    assert.equal((await request(state.port).result).status, 200);
    assert.deepEqual(state.seen, ['first', 'queued', 'replacement']);
  } finally {
    state.release.resolve([]);
    state.socket.destroy();
  }
});
for (const terminal of ['finish', 'close']) {
  test(`buffered failed response retains admission until actual ${terminal}`, {
    timeout: 60000,
  }, async (t) => {
    const state = await pipeline(t, true);
    try {
      assert.equal(
        (await request(state.port).result).status,
        503,
        'buffered failure must still occupy the second search slot',
      );
      assert.deepEqual(state.seen, ['first', 'queued']);
      const done = new Promise<void>((resolve) => state.response.once(terminal, resolve));
      if (terminal === 'finish') state.release.resolve([]);
      else state.socket.destroy();
      await done;
      assert.equal((await request(state.port).result).status, 200);
      assert.deepEqual(state.seen, ['first', 'queued', 'replacement']);
    } finally {
      state.release.resolve([]);
      state.socket.destroy();
    }
  });
}

test('end callback write error destroys failed delivery rather than counting success', {
  timeout: 60000,
}, async (t) => {
  const completed = deferred<http.ServerResponse>();
  const original = http.ServerResponse.prototype.end;
  t.mock.method(
    http.ServerResponse.prototype,
    'end',
    function (this: http.ServerResponse, ...args: any[]) {
      if (this.req.headers['x-case'] !== 'write-error') return Reflect.apply(original, this, args);
      args.at(-1)(new Error('deterministic write failure'));
      completed.resolve(this);
      return this;
    },
  );
  const api: SearchClient = {
    search: async () => [],
    serverStatus: async () => {
      throw new Error('unused');
    },
    close() {},
  };
  const { port } = await listen(t, api);
  const pending = request(port, 'x', '/search', 'write-error');
  try {
    const response = await completed.promise;
    assert.equal(response.destroyed, true);
  } finally {
    pending.req.destroy();
    await pending.result.catch(() => {});
  }
});

test('buffered error delivery cannot outlive the original deadline or gain a flush grace', {
  timeout: 60000,
}, async (t) => {
  let now = 0;
  t.mock.method(performance, 'now', () => now);
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const state = await pipeline(t, true);
  try {
    now = 10000;
    t.mock.timers.tick(10000);
    assert.equal(state.response.destroyed, true);
    assert.equal(state.signals.get('queued')!.aborted, true);
  } finally {
    state.release.resolve([]);
    state.socket.destroy();
  }
});
