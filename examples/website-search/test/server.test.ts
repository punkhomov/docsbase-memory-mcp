import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import http, { type ClientRequest } from 'node:http';
import { once } from 'node:events';
import { performance } from 'node:perf_hooks';
import { DocsbaseError, type CallOptions, type ServerStatus } from '@docsbase/client';
import { resolveWebsiteConfig } from '../config.ts';
import { createWebsiteServer, type SearchClient } from '../server.ts';

function deferred<T = void>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function input() {
  return {
    http: {
      host: '127.0.0.1',
      port: 0,
      maxConnections: 16,
      searchActiveRequests: 1,
      searchPendingRequests: 1,
      controlActiveRequests: 1,
      controlPendingRequests: 1,
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
        process.platform === 'win32' ? String.raw`\\.\pipe\unused-http-test` : '/unused-http-test',
      projectId: 7,
      expectedBuildId: 'docsbase test',
      expectedSchemaVersion: 1,
      settings: {
        searchConnections: 1,
        controlConnections: 1,
        queueLimit: 1,
        requestTimeoutMs: 10000,
      },
    },
  };
}
const ready = {
  ready: true,
  reason: null,
  index_generation: 3,
  restart_required: false,
} as ServerStatus;
const healthy = (): SearchClient => ({
  search: async () => [],
  serverStatus: async () => ready,
  close() {},
});
async function setup(t: TestContext, api: SearchClient = healthy(), config = input()) {
  const service = createWebsiteServer(resolveWebsiteConfig(config), api);
  service.server.listen(0, '127.0.0.1');
  await once(service.server, 'listening');
  const address = service.server.address();
  assert.ok(address && typeof address === 'object');
  t.after(() => service.close());
  return { service, port: address.port };
}
function request(
  port: number,
  path = '/search',
  body = '{"query":"widgets"}',
  method = 'POST',
  headers: http.OutgoingHttpHeaders = {},
) {
  let req!: ClientRequest;
  const result = new Promise<{ status: number; body: string }>((resolve, reject) => {
    req = http.request(
      {
        host: '127.0.0.1',
        port,
        path,
        method,
        agent: false,
        headers: { 'content-type': 'application/json', ...headers },
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
    req.end(body);
  });
  void result.catch(() => {});
  return { req, result };
}

test('complete owner policy rejects omitted, invalid and unknown controls without I/O', () => {
  for (const key of Object.keys(input().http)) {
    const config: any = input();
    delete config.http[key];
    assert.throws(() => resolveWebsiteConfig(config));
  }
  for (const change of [
    { maxBodyBytes: 0 },
    { searchPendingRequests: -1 },
    { requestTimeoutMs: Infinity },
    { port: 65536 },
    { extra: 1 },
  ]) {
    assert.throws(() =>
      resolveWebsiteConfig({
        ...input(),
        http: { ...input().http, ...change },
      }),
    );
  }
});

test('fixed HTTP routes expose search/readiness and preserve completed request close', async (t) => {
  const arrived = deferred<CallOptions | undefined>();
  const release = deferred<[]>();
  const api = healthy();
  api.search = async (query, options, call) => {
    assert.equal(query, 'widgets');
    assert.equal(options?.limit, 10);
    arrived.resolve(call);
    return release.promise;
  };
  const { port, service } = await setup(t, api);
  const bodyClosed = deferred();
  service.server.once('request', (req) => req.once('close', () => bodyClosed.resolve()));
  const pending = request(port);
  const call = await arrived.promise;
  await bodyClosed.promise;
  assert.equal(call?.signal?.aborted, false, 'completed body close is not disconnect');
  assert.ok(call?.budget && call.budget.deadlineAtMs > call.budget.startedAtMs);
  release.resolve([]);
  assert.equal((await pending.result).status, 200);
  const status = await request(port, '/ready', '', 'GET').result;
  assert.equal(status.status, 200);
  assert.deepEqual(JSON.parse(status.body), {
    ready: true,
    reason: null,
    index_generation: 3,
    restart_required: false,
  });
});

test('body/header/result caps reject before retaining oversized data or dispatch', async (t) => {
  let searches = 0;
  const api = healthy();
  api.search = async () => {
    searches++;
    return [];
  };
  const { port } = await setup(t, api);
  assert.equal((await request(port, '/search', 'x'.repeat(513)).result).status, 413);
  assert.equal(
    (
      await request(port, '/search', '{}', 'POST', {
        'x-large': 'x'.repeat(5000),
      }).result
    ).status,
    431,
  );
  assert.equal(
    (
      await request(
        port,
        '/search',
        '{}',
        'POST',
        Object.fromEntries(Array.from({ length: 17 }, (_, i) => [`x-${i}`, 'x'])),
      ).result
    ).status,
    431,
  );
  assert.equal(searches, 0);
  api.search = async () => [
    {
      chunk_id: '1',
      path: 'a',
      heading_path: '',
      lines: [1, 1],
      score: 1,
      snippet: 'x'.repeat(5000),
    },
  ];
  assert.equal((await request(port).result).status, 502);
});

test('invalid methods/options and browser project/tool/provider controls never dispatch', async (t) => {
  let searches = 0;
  const api = healthy();
  api.search = async () => {
    searches++;
    return [];
  };
  const { port } = await setup(t, api);
  assert.equal((await request(port, '/search', '', 'GET').result).status, 405);
  assert.equal((await request(port, '/other', '', 'GET').result).status, 404);
  assert.equal((await request(port, '/ready?project=8', '', 'GET').result).status, 404);
  for (const body of [
    '{',
    '{"query":1}',
    JSON.stringify({ query: 'x'.repeat(129) }),
    ...['project_id', 'root', 'tool', 'provider'].map((key) =>
      JSON.stringify({ query: 'x', [key]: 'other' }),
    ),
    ...[{ rerank: true }, { limit: 11 }, { explain: 'yes' }, { tags: [1] }, { extra: true }].map(
      (options) => JSON.stringify({ query: 'x', options }),
    ),
  ]) {
    assert.equal((await request(port, '/search', body).result).status, 400, body);
  }
  assert.equal(searches, 0);
});

test('bounded FIFO search queue cancels a disconnected waiter while readiness uses its control lane', async (t) => {
  const entered = deferred();
  const release = deferred<[]>();
  const seen: string[] = [];
  const api = healthy();
  api.search = async (query) => {
    seen.push(query);
    entered.resolve();
    return release.promise;
  };
  const { port, service } = await setup(t, api);
  const first = request(port);
  await entered.promise;
  const observedQueue = deferred();
  service.server.once('request', () => observedQueue.resolve());
  const removed = request(port, '/search', '{"query":"removed"}');
  await observedQueue.promise;
  assert.equal((await request(port, '/search', '{"query":"overflow"}').result).status, 503);
  assert.equal((await request(port, '/ready', '', 'GET').result).status, 200);
  removed.req.destroy();
  await assert.rejects(removed.result);
  // The response close edge retires the waiter; a later response proves the loop
  // processed that edge before another request is admitted.
  await request(port, '/ready', '', 'GET').result;
  const replacement = request(port, '/search', '{"query":"replacement"}');
  release.resolve([]);
  assert.equal((await first.result).status, 200);
  assert.equal((await replacement.result).status, 200);
  assert.deepEqual(seen, ['widgets', 'replacement']);
});

test('aborted body and active search release resources and propagate the original signal', async (t) => {
  const entered = deferred<AbortSignal>();
  const aborted = deferred();
  const api = healthy();
  api.search = async (_query, _options, call) => {
    entered.resolve(call!.signal!);
    return new Promise((_, reject) =>
      call!.signal!.addEventListener(
        'abort',
        () => {
          aborted.resolve();
          reject(call!.signal!.reason);
        },
        { once: true },
      ),
    );
  };
  const { port, service } = await setup(t, api);
  const received = deferred();
  service.server.once('request', () => received.resolve());
  const partial = http.request({
    host: '127.0.0.1',
    port,
    method: 'POST',
    path: '/search',
    headers: { 'content-length': 100, 'content-type': 'application/json' },
    agent: false,
  });
  partial.on('error', () => {});
  partial.write('{');
  await received.promise;
  partial.destroy();
  await request(port, '/ready', '', 'GET').result;
  const active = request(port);
  const signal = await entered.promise;
  active.req.destroy();
  await assert.rejects(active.result);
  await aborted.promise;
  assert.equal(signal.aborted, true);
});

test('JSON preparation consuming the original HTTP deadline never reaches the SDK', async (t) => {
  let now = 0;
  let calls = 0;
  t.mock.method(performance, 'now', () => now);
  const parse = JSON.parse;
  t.mock.method(JSON, 'parse', (...args: Parameters<typeof JSON.parse>) => {
    const result: unknown = parse(...args);
    if (args[0].includes('spentwidget')) now = 10001;
    return result;
  });
  const api = healthy();
  api.search = async () => {
    calls++;
    return [];
  };
  const { port } = await setup(t, api);
  assert.equal((await request(port, '/search', '{"query":"spentwidget"}').result).status, 504);
  assert.equal(calls, 0);
});

test('typed SDK failures are mapped to bounded public errors without replay or daemon messages', async (t) => {
  const api = healthy();
  let calls = 0;
  const { port } = await setup(t, api);
  for (const [error, status] of [
    [new DocsbaseError('overloaded', 'private'), 503],
    [new DocsbaseError('transport', 'private'), 503],
    [new DocsbaseError('daemon', 'private', { code: -32010 }), 503],
    [new DocsbaseError('daemon', 'private', { code: -32014 }), 400],
    [new DocsbaseError('protocol', 'private'), 502],
    [new DocsbaseError('deadline', 'private'), 504],
  ] as const) {
    api.search = async () => {
      calls++;
      throw error;
    };
    const response = await request(port).result;
    assert.equal(response.status, status);
    assert.equal(response.body.includes('private'), false);
  }
  assert.equal(calls, 6);
});
