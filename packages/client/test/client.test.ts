import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { performance } from 'node:perf_hooks';
import type { Socket } from 'node:net';
import { DocsbaseClient } from '../src/index.ts';
import { peer, options, hello, ok, line, deferred } from './peer.ts';

const result = { kind: 'ToolResult', data: { value: [] } };
const status = {
  kind: 'ToolResult',
  data: {
    value: {
      restart_required: false,
      notice: null,
      projects: [
        {
          id: 7,
          name: 'docs',
          root: '/docs',
          status: 'indexed',
          docs: 3,
          chunks: 4,
          index_generation: 1,
          watched: false,
        },
        {
          id: 8,
          name: 'other',
          root: '/other',
          status: 'indexed',
          docs: 1,
          chunks: 1,
          index_generation: 2,
          watched: false,
        },
      ],
    },
  },
};
const parked = <T>(promise: Promise<T>): Promise<T> => {
  void promise.catch(() => {});
  return promise;
};
async function clientPeer(
  t: TestContext,
  handle: (request: Record<string, any>, socket: Socket) => void,
  settings = {
    searchConnections: 1,
    controlConnections: 1,
    queueLimit: 1,
    requestTimeoutMs: 10000,
  },
) {
  const endpoint = await peer(t, (request, socket) => {
    if (request.method === 'Hello') socket.write(line(hello));
    else if (request.method === 'BindReadOnly') socket.write(line(ok));
    else handle(request, socket);
  });
  const client = new DocsbaseClient({ ...options(endpoint), settings });
  t.after(() => client.close());
  return client;
}

test('bounded FIFO search admission and independent project-only status', {
  timeout: 10000,
}, async (t) => {
  const arrived = deferred<Socket>();
  const seen: string[] = [];
  const client = await clientPeer(t, (request, socket) => {
    if (request.method === 'CallTool') {
      assert.equal(request.params.name, 'status');
      socket.write(line(status));
      return;
    }
    seen.push(request.params.args.query);
    if (seen.length === 1) arrived.resolve(socket);
    else socket.write(line(result));
  });
  const first = parked(client.search('first'));
  const socket = await arrived.promise;
  const second = parked(client.search('second'));
  await assert.rejects(client.search('third'), { kind: 'overloaded' });
  const info = await client.status();
  assert.equal(info.project.id, 7);
  assert.equal(info.project.index_generation, 1);
  assert.equal('projects' in info, false);
  assert.deepEqual(seen, ['first']);
  socket.write(line(result));
  await Promise.all([first, second]);
  assert.deepEqual(seen, ['first', 'second']);
  assert.equal(client.metadata?.build_id, 'docsbase test');
});

test('cancelled queued waiter frees capacity and is never dispatched', {
  timeout: 10000,
}, async (t) => {
  const arrived = deferred<Socket>();
  const seen: string[] = [];
  const client = await clientPeer(t, (request, socket) => {
    seen.push(request.params.args.query);
    if (seen.length === 1) arrived.resolve(socket);
    else socket.write(line(result));
  });
  const first = parked(client.search('held'));
  const socket = await arrived.promise;
  const cancellation = new AbortController();
  const cancelled = parked(client.search('cancelled', {}, { signal: cancellation.signal }));
  cancellation.abort();
  await assert.rejects(cancelled, { kind: 'cancelled' });
  const next = parked(client.search('next'));
  socket.write(line(result));
  await Promise.all([first, next]);
  assert.deepEqual(seen, ['held', 'next']);
});

test('active abort closes interrupted connection, next call handshakes again', {
  timeout: 10000,
}, async (t) => {
  const arrived = deferred();
  let hellos = 0;
  const seen: string[] = [];
  const endpoint = await peer(t, (request, socket) => {
    if (request.method === 'Hello') {
      hellos++;
      socket.write(line(hello));
    } else if (request.method === 'BindReadOnly') socket.write(line(ok));
    else {
      seen.push(request.params.args.query);
      if (seen.length === 1) arrived.resolve();
      else socket.write(line(result));
    }
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  const cancellation = new AbortController();
  const first = parked(client.search('cancel', {}, { signal: cancellation.signal }));
  await arrived.promise;
  cancellation.abort();
  await assert.rejects(first, { kind: 'cancelled' });
  await client.search('later');
  assert.equal(hellos, 2);
  assert.deepEqual(seen, ['cancel', 'later']);
});

test('transport failure never replays a dispatched search; later call reconnects', {
  timeout: 10000,
}, async (t) => {
  let searches = 0,
    hellos = 0;
  const endpoint = await peer(t, (request, socket) => {
    if (request.method === 'Hello') {
      hellos++;
      socket.write(line(hello));
    } else if (request.method === 'BindReadOnly') socket.write(line(ok));
    else {
      searches++;
      if (searches === 1) socket.destroy();
      else socket.write(line(result));
    }
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  await assert.rejects(client.search('once'), { kind: 'transport' });
  assert.equal(searches, 1);
  assert.equal(hellos, 1);
  await client.search('new operation');
  assert.equal(searches, 2);
  assert.equal(hellos, 2);
});

test('daemon errors preserve code/instruction and keep valid channel reusable', {
  timeout: 10000,
}, async (t) => {
  let count = 0;
  const client = await clientPeer(t, (_request, socket) => {
    socket.write(
      line(
        ++count === 1
          ? {
              kind: 'Error',
              data: { code: -32014, message: 'invalid query', instruction: 'fix query' },
            }
          : result,
      ),
    );
  });
  await assert.rejects(client.search('bad'), {
    kind: 'daemon',
    code: -32014,
    instruction: 'fix query',
    message: 'invalid query',
  });
  await client.search('good');
  assert.equal(count, 2);
});

test('options are snapshotted before queueing and omitted defaults stay absent', {
  timeout: 10000,
}, async (t) => {
  const arrived = deferred<Socket>();
  const args: unknown[] = [];
  const client = await clientPeer(t, (request, socket) => {
    args.push(request.params.args);
    if (args.length === 1) arrived.resolve(socket);
    else socket.write(line(result));
  });
  const first = parked(client.search('held'));
  const socket = await arrived.promise;
  const mutable = { tags: ['original'], snippet: true };
  const second = parked(client.search('second', mutable));
  mutable.tags[0] = 'changed';
  mutable.snippet = false;
  socket.write(line(result));
  await Promise.all([first, second]);
  assert.deepEqual(args, [
    { query: 'held' },
    { query: 'second', tags: ['original'], snippet: true },
  ]);
});

test('literal negotiation and option validation happen before Search dispatch', {
  timeout: 10000,
}, async (t) => {
  const methods: string[] = [];
  const endpoint = await peer(t, (request, socket) => {
    methods.push(request.method);
    socket.write(
      line(
        request.method === 'Hello'
          ? {
              ...hello,
              data: { ...hello.data, capabilities: ['search-context-v1', 'panel-read-v1'] },
            }
          : ok,
      ),
    );
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  await assert.rejects(client.search('x', { query_mode: 'literal' }), { kind: 'protocol' });
  assert.deepEqual(methods, ['Hello', 'BindReadOnly']);
  for (const invalid of [
    { query_mode: 'other' },
    { project: '/elsewhere' },
    { limit: -1 },
    { tags: [1] },
  ]) {
    await assert.rejects(client.search('x', invalid as any), { kind: 'validation' });
  }
  assert.deepEqual(methods, ['Hello', 'BindReadOnly']);
});

test('preprocessing consumes original deadline before any socket admission', {
  timeout: 10000,
}, async (t) => {
  let now = 0,
    calls = 0;
  t.mock.method(performance, 'now', () => now);
  const client = await clientPeer(t, (_request, socket) => {
    calls++;
    socket.write(line(result));
  });
  await assert.rejects(
    client.search('x', {
      get limit() {
        now += 10000;
        return 1;
      },
    }),
    { kind: 'deadline' },
  );
  await assert.rejects(
    client.status({
      get signal() {
        now += 10000;
        return undefined;
      },
    } as any),
    { kind: 'deadline' },
  );
  assert.equal(client.metadata, undefined);
  assert.equal(calls, 0);
});

test('search context includes preprocessing and Hello/bind under one budget', {
  timeout: 10000,
}, async (t) => {
  let now = 0;
  let context: unknown;
  t.mock.method(performance, 'now', () => now);
  const endpoint = await peer(t, (request, socket) => {
    if (request.method === 'Hello') {
      now += 11;
      socket.write(line(hello));
    } else if (request.method === 'BindReadOnly') {
      now += 13;
      socket.write(line(ok));
    } else {
      context = request.params.context;
      socket.write(line(result));
    }
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  await client.search('x', {
    get limit() {
      now += 7;
      return 1;
    },
  });
  assert.deepEqual(context, { remaining_ms: 9969, elapsed_ms: 31, queue_ms: 0 });
});

for (const method of ['Search', 'CallTool'] as const) {
  test(`${method} final serialization cannot write after the original deadline`, {
    timeout: 10000,
  }, async (t) => {
    let now = 100,
      serializations = 0;
    const methods: string[] = [];
    const closed = deferred();
    t.mock.method(performance, 'now', () => now);
    t.mock.timers.enable({ apis: ['setTimeout'] });
    const stringify = JSON.stringify;
    t.mock.method(JSON, 'stringify', (...args: Parameters<typeof JSON.stringify>) => {
      const encoded = stringify(...args);
      const [value] = args;
      if (
        value?.method === method &&
        (method !== 'Search' || value.params.context.remaining_ms !== Number.MAX_SAFE_INTEGER)
      ) {
        serializations++;
        now = 10100;
      }
      return encoded;
    });
    const endpoint = await peer(t, (request, socket) => {
      methods.push(request.method);
      if (request.method === 'Hello') socket.once('close', () => closed.resolve());
      socket.write(
        line(request.method === 'Hello' ? hello : request.method === 'BindReadOnly' ? ok : result),
      );
    });
    const client = new DocsbaseClient(options(endpoint));
    t.after(() => client.close());
    await assert.rejects(method === 'Search' ? client.search('x') : client.status(), {
      kind: 'deadline',
    });
    await closed.promise;
    assert.equal(serializations, 1);
    assert.deepEqual(methods, ['Hello', 'BindReadOnly']);
  });

  test(`${method} Hello decoding cannot bind after the original deadline`, {
    timeout: 10000,
  }, async (t) => {
    let now = 100,
      decodes = 0;
    const methods: string[] = [];
    const closed = deferred();
    t.mock.method(performance, 'now', () => now);
    t.mock.timers.enable({ apis: ['setTimeout'] });
    const parse = JSON.parse;
    t.mock.method(JSON, 'parse', (...args: Parameters<typeof JSON.parse>) => {
      const decoded = parse(...args);
      if (decoded?.kind === 'Hello') {
        decodes++;
        now = 10100;
      }
      return decoded;
    });
    const endpoint = await peer(t, (request, socket) => {
      methods.push(request.method);
      if (request.method === 'Hello') socket.once('close', () => closed.resolve());
      socket.write(line(request.method === 'Hello' ? hello : ok));
    });
    const client = new DocsbaseClient(options(endpoint));
    t.after(() => client.close());
    await assert.rejects(method === 'Search' ? client.search('x') : client.status(), {
      kind: 'deadline',
    });
    await closed.promise;
    assert.equal(decodes, 1);
    assert.deepEqual(methods, ['Hello']);
  });
}

test('deadline interrupts pending Hello instead of starting a fresh exchange budget', {
  timeout: 10000,
}, async (t) => {
  let now = 0;
  const arrived = deferred();
  const methods: string[] = [];
  t.mock.method(performance, 'now', () => now);
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const endpoint = await peer(t, (request) => {
    methods.push(request.method);
    arrived.resolve();
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  const pending = parked(client.search('x'));
  await arrived.promise;
  now = 10000;
  t.mock.timers.tick(10000);
  await assert.rejects(pending, { kind: 'deadline' });
  assert.deepEqual(methods, ['Hello']);
});

test('queued expiry never dispatches work and cleans up active/pending operations', {
  timeout: 10000,
}, async (t) => {
  let now = 0,
    searches = 0;
  const arrived = deferred();
  t.mock.method(performance, 'now', () => now);
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const client = await clientPeer(t, () => {
    searches++;
    arrived.resolve();
  });
  const first = parked(client.search('held'));
  await arrived.promise;
  const second = parked(client.search('queued'));
  now = 10000;
  t.mock.timers.tick(10000);
  await assert.rejects(first, { kind: 'deadline' });
  await assert.rejects(second, { kind: 'deadline' });
  assert.equal(searches, 1);
});

test('close rejects active, pending and future calls, without stopping daemon', {
  timeout: 10000,
}, async (t) => {
  const arrived = deferred();
  const methods: string[] = [];
  const client = await clientPeer(t, (request) => {
    methods.push(request.method);
    arrived.resolve();
  });
  const first = parked(client.search('held'));
  await arrived.promise;
  const second = parked(client.search('queued'));
  client.close();
  client.close();
  await assert.rejects(first, { kind: 'closed' });
  await assert.rejects(second, { kind: 'closed' });
  await assert.rejects(client.status(), { kind: 'closed' });
  await assert.rejects(client.search('later'), { kind: 'closed' });
  assert.deepEqual(methods, ['Search']);
});

test('configured parallel slots run together and preserve FIFO past cancelled waiter', {
  timeout: 10000,
}, async (t) => {
  const arrived = deferred();
  const sockets: Socket[] = [];
  const seen: string[] = [];
  const client = await clientPeer(
    t,
    (request, socket) => {
      seen.push(request.params.args.query);
      if (seen.length <= 2) {
        sockets.push(socket);
        if (seen.length === 2) arrived.resolve();
      } else socket.write(line(result));
    },
    { searchConnections: 2, controlConnections: 1, queueLimit: 2, requestTimeoutMs: 10000 },
  );
  const a = parked(client.search('a')),
    b = parked(client.search('b'));
  await arrived.promise;
  const cancellation = new AbortController();
  const removed = parked(client.search('removed', {}, { signal: cancellation.signal }));
  const c = parked(client.search('c'));
  cancellation.abort();
  await assert.rejects(removed, { kind: 'cancelled' });
  const d = parked(client.search('d'));
  await assert.rejects(client.search('overflow'), { kind: 'overloaded' });
  sockets[0]!.write(line(result));
  await Promise.all([a, c, d]);
  sockets[1]!.write(line(result));
  await b;
  assert.deepEqual(seen.slice(0, 2).sort(), ['a', 'b']);
  assert.deepEqual(seen.slice(2), ['c', 'd']);
});

test('zero pending policy rejects immediately while pre-aborted calls do no preparation', {
  timeout: 10000,
}, async (t) => {
  const arrived = deferred();
  const client = await clientPeer(t, () => arrived.resolve(), {
    searchConnections: 1,
    controlConnections: 1,
    queueLimit: 0,
    requestTimeoutMs: 10000,
  });
  const cancellation = new AbortController();
  cancellation.abort();
  let prepared = false;
  await assert.rejects(
    client.search(
      'x',
      {
        get limit() {
          prepared = true;
          return 1;
        },
      },
      { signal: cancellation.signal },
    ),
    { kind: 'cancelled' },
  );
  assert.equal(prepared, false);
  const held = parked(client.search('held'));
  await arrived.promise;
  await assert.rejects(client.search('no queue'), { kind: 'overloaded' });
  client.close();
  await assert.rejects(held, { kind: 'closed' });
});

test('malformed search payload poisons channel and stale idle channels are not reused', {
  timeout: 10000,
}, async (t) => {
  let hellos = 0,
    queries = 0;
  const endpoint = await peer(t, (request, socket) => {
    if (request.method === 'Hello') {
      hellos++;
      socket.write(line(hello));
    } else if (request.method === 'BindReadOnly') socket.write(line(ok));
    else {
      queries++;
      socket.write(line(queries === 1 ? { kind: 'ToolResult', data: { value: [{}] } } : result));
    }
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  await assert.rejects(client.search('bad payload'), { kind: 'protocol' });
  assert.deepEqual(await client.search('new'), []);
  assert.equal(hellos, 2);
});

test('queued context accounts only actual FIFO wait without resetting original deadline', {
  timeout: 10000,
}, async (t) => {
  let now = 0;
  t.mock.method(performance, 'now', () => now);
  const held = deferred<Socket>();
  let secondContext: unknown;
  let searches = 0;
  const client = await clientPeer(t, (request, socket) => {
    if (++searches === 1) held.resolve(socket);
    else {
      secondContext = request.params.context;
      socket.write(line(result));
    }
  });
  const first = parked(client.search('held'));
  const socket = await held.promise;
  now = 20;
  const second = parked(client.search('queued'));
  now = 50;
  socket.write(line(result));
  await Promise.all([first, second]);
  assert.deepEqual(secondContext, { remaining_ms: 9970, elapsed_ms: 30, queue_ms: 30 });
});

test('plain owned tags cannot invoke user toJSON or retain a changed queued payload', {
  timeout: 10000,
}, async (t) => {
  let serializations = 0,
    captured: unknown;
  const client = await clientPeer(t, (request, socket) => {
    captured = request.params.args;
    socket.write(line(result));
  });
  const tags = Object.assign(['safe'], {
    toJSON() {
      serializations++;
      return ['changed'];
    },
  });
  await client.search('query', { tags });
  assert.equal(serializations, 0);
  assert.deepEqual(captured, { query: 'query', tags: ['safe'] });
});
