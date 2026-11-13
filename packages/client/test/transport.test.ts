import test from 'node:test';
import assert from 'node:assert/strict';
import { setImmediate } from 'node:timers/promises';
import { Connection, type OperationGuard } from '../src/connection.ts';
import { abortError } from '../src/types.ts';
import { resolveClientOptions } from '../src/settings.ts';
import { decodeResponse, searchResult, MAX_FRAME_BYTES } from '../src/protocol.ts';
import { peer, hello, ok, options, line, deferred } from './peer.ts';

const control = { method: 'CallTool', params: { name: 'status', args: {} } } as const;
// These transport-only calls use an explicit caller-owned abort guard, without a deadline.
const operation = (signal = new AbortController().signal): OperationGuard => ({
  signal,
  check() {
    if (signal.aborted) throw abortError(signal);
  },
});
const nativeEndpoint =
  process.platform === 'win32' ? String.raw`\\.\pipe\docsbase-client-test` : '/tmp/client.sock';

test('native Hello + read-only binding, including fragmented UTF-8', {
  timeout: 10000,
}, async (t) => {
  const methods: string[] = [];
  const endpoint = await peer(t, (request, socket) => {
    methods.push(request.method);
    if (request.method === 'Hello') {
      assert.equal(request.params.build_id, 'docsbase test');
      const bytes = Buffer.from(
        line({
          ...hello,
          data: { ...hello.data, capabilities: [...hello.data.capabilities, 'привет'] },
        }),
      );
      void (async () => {
        for (let offset = 0; offset < bytes.length; offset += 3) {
          socket.write(bytes.subarray(offset, offset + 3));
          await setImmediate();
        }
      })();
    } else {
      assert.deepEqual(request, {
        method: 'BindReadOnly',
        params: { pid: process.pid, project_id: 7 },
      });
      socket.write(line(ok));
    }
  });
  const connection = await Connection.open(resolveClientOptions(options(endpoint)), operation());
  t.after(() => connection.close());
  assert.equal(connection.metadata.schema_version, 2);
  assert.ok(connection.metadata.capabilities.includes('привет'));
  assert.deepEqual(methods, ['Hello', 'BindReadOnly']);
});

for (const [name, data] of Object.entries({
  version: { ...hello.data, protocol_version: 2 },
  build: { ...hello.data, build_id: 'other' },
  schema: { ...hello.data, schema_version: 3 },
  zeroSchema: { ...hello.data, schema_version: 0 },
  negativeSchema: { ...hello.data, schema_version: -1 },
  fractionalSchema: { ...hello.data, schema_version: 1.5 },
  unsafeSchema: { ...hello.data, schema_version: Number.MAX_SAFE_INTEGER + 1 },
  overflowSchema: { ...hello.data, schema_version: 0x100000000 },
  stringSchema: { ...hello.data, schema_version: '2' },
  missingSchema: {
    protocol_version: 1,
    build_id: 'docsbase test',
    capabilities: hello.data.capabilities,
  },
  capability: { ...hello.data, capabilities: ['panel-read-v1'] },
}))
  test(`rejects Hello ${name} before bind`, { timeout: 10000 }, async (t) => {
    const requests: string[] = [];
    const closed = deferred();
    const endpoint = await peer(t, (request, socket) => {
      requests.push(request.method);
      socket.once('close', () => closed.resolve());
      socket.write(line({ kind: 'Hello', data }));
    });
    await assert.rejects(Connection.open(resolveClientOptions(options(endpoint)), operation()), {
      kind: 'protocol',
    });
    await closed.promise;
    assert.deepEqual(requests, ['Hello']);
  });

test('settings require complete validated immutable policy and schema', () => {
  const source = options(nativeEndpoint);
  const resolved = resolveClientOptions(source);
  source.settings.queueLimit = 99;
  assert.equal(resolved.settings.queueLimit, 2);
  assert.ok(Object.isFrozen(resolved.settings));
  for (const bad of [undefined, '2', 0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, 0x100000000]) {
    assert.throws(() => resolveClientOptions({ ...source, expectedSchemaVersion: bad } as any), {
      kind: 'validation',
    });
  }
  assert.throws(() => resolveClientOptions({ ...source, settings: {} } as any), {
    kind: 'validation',
  });
  assert.throws(() => resolveClientOptions({ ...source, endpoint: 'tcp://localhost:1234' }), {
    kind: 'validation',
  });
  assert.throws(
    () => resolveClientOptions({ ...source, endpoint: String.raw`\\remote\pipe\docsbase` }),
    { kind: 'validation' },
  );
  assert.throws(() => resolveClientOptions({ ...source, expectedSchemaVersion: 0x100000000 }), {
    kind: 'validation',
  });
});

test('Windows endpoints require the canonical local named-pipe namespace', () => {
  const localPipe = String.raw`\\.\pipe\docsbase-client-test`;
  assert.equal(resolveClientOptions(options(localPipe), 'win32').endpoint, localPipe);
  for (const endpoint of [
    '//remote/pipe/docsbase',
    String.raw`\\remote\pipe\docsbase`,
    '/tmp/client.sock',
    String.raw`C:\docsbase.sock`,
    '//./pipe/docsbase',
    String.raw`\\?\pipe\docsbase`,
    '\\\\.\\pipe\\',
    localPipe + '\0',
  ]) {
    assert.throws(
      () => resolveClientOptions(options(endpoint), 'win32'),
      { kind: 'validation' },
      endpoint,
    );
  }
});

for (const platform of ['linux', 'darwin'] as const) {
  test(`${platform} endpoints require an absolute Unix socket path`, () => {
    const endpoint = '/tmp/docsbase.sock';
    assert.equal(resolveClientOptions(options(endpoint), platform).endpoint, endpoint);
    for (const invalid of [
      'docsbase.sock',
      'tcp://localhost:1234',
      String.raw`\\.\pipe\docsbase`,
      String.raw`\\remote\pipe\docsbase`,
      endpoint + '\0',
    ]) {
      assert.throws(
        () => resolveClientOptions(options(invalid), platform),
        { kind: 'validation' },
        invalid,
      );
    }
  });
}

for (const [name, response] of Object.entries({
  malformed: '{x}\n',
  incomplete: '{"kind":"ToolResult"}',
  oversized: ' '.repeat(MAX_FRAME_BYTES + 1),
  extra: line(ok) + line(ok),
  unknownKind: '{"kind":"Future"}\n',
}))
  test(`poisons connection on ${name} frame`, { timeout: 10000 }, async (t) => {
    const endpoint = await peer(t, (request, socket) => {
      if (request.method === 'Hello') socket.write(line(hello));
      else if (request.method === 'BindReadOnly') socket.write(line(ok));
      else socket.end(response);
    });
    const connection = await Connection.open(resolveClientOptions(options(endpoint)), operation());
    t.after(() => connection.close());
    await assert.rejects(connection.exchange(control, operation()));
    assert.equal(connection.alive, false);
    await assert.rejects(connection.exchange(control, operation()));
  });

test('outbound frame cap checked before write', { timeout: 10000 }, async (t) => {
  let calls = 0;
  const endpoint = await peer(t, (request, socket) => {
    calls++;
    socket.write(line(request.method === 'Hello' ? hello : ok));
  });
  const connection = await Connection.open(resolveClientOptions(options(endpoint)), operation());
  t.after(() => connection.close());
  await assert.rejects(
    connection.exchange(
      {
        method: 'Search',
        params: {
          args: { query: 'a'.repeat(MAX_FRAME_BYTES) },
          context: { remaining_ms: 100, elapsed_ms: 0, queue_ms: 0 },
        },
      },
      operation(),
    ),
    { kind: 'validation' },
  );
  assert.equal(calls, 2);
});

test('u64 tokens preserve exact identity and reject non-u64 input', () => {
  const hit = (id: string) =>
    `{"kind":"ToolResult","data":{"value":[{"chunk_id":${id},"path":"guide.md","heading_path":"Guide","lines":[1,2],"score":0.5}]}}`;
  for (const id of ['0', '9007199254740991', '9007199254740992', '18446744073709551615']) {
    const response = decodeResponse(Buffer.from(hit(id)));
    assert.equal(response.kind, 'ToolResult');
    if (response.kind !== 'ToolResult') throw new Error('unexpected');
    const result = searchResult(response.data.value);
    assert.ok(Array.isArray(result));
    assert.equal(result[0]?.chunk_id, id);
  }
  for (const id of ['18446744073709551616', '-1', '-0', '1.5', '1e3', '1e0', '"7"']) {
    assert.throws(() => searchResult((decodeResponse(Buffer.from(hit(id))) as any).data.value), {
      kind: 'protocol',
    });
  }
  assert.throws(
    () =>
      decodeResponse(Buffer.from('{"kind":"ToolResult","data":{"value":{"id":9007199254740992}}}')),
    { kind: 'protocol' },
  );
});

test('raw unsafe score integers are rejected before rounding', () => {
  const wire =
    '{"kind":"ToolResult","data":{"value":[{"chunk_id":1,"path":"guide.md","heading_path":"Guide","lines":[1,2],"score":9007199254740993}]}}';
  assert.throws(() => decodeResponse(Buffer.from(wire)), { kind: 'protocol' });
});

test('safe integer and fractional citation scores remain valid', () => {
  for (const score of [0, -1, 0.5, Number.MAX_SAFE_INTEGER]) {
    const wire = `{"kind":"ToolResult","data":{"value":[{"chunk_id":1,"path":"guide.md","heading_path":"Guide","lines":[1,2],"score":${score}}]}}`;
    const response = decodeResponse(Buffer.from(wire));
    assert.equal(response.kind, 'ToolResult');
    if (response.kind !== 'ToolResult') throw new Error('unexpected');
    const result = searchResult(response.data.value);
    assert.ok(Array.isArray(result));
    assert.equal(result[0]?.score, score);
  }
});

test('runtime citation validation rejects invalid types while preserving additions', () => {
  const base = {
    chunk_id: '1',
    path: 'a.md',
    heading_path: 'A',
    lines: [1, 2],
    score: 0.3,
    extra: 'kept',
  };
  assert.deepEqual(searchResult([base]), [base]);
  for (const bad of [
    { ...base, path: 1 },
    { ...base, lines: [1] },
    { ...base, lines: [-1, 2] },
    { ...base, score: Infinity },
    { ...base, snippet: false },
  ]) {
    assert.throws(() => searchResult([bad]), { kind: 'protocol' });
  }
});

test('aborted Hello closes exchange without binding', { timeout: 10000 }, async (t) => {
  const controller = new AbortController();
  const requests: string[] = [];
  const endpoint = await peer(t, (request) => {
    requests.push(request.method);
    controller.abort();
  });
  await assert.rejects(
    Connection.open(resolveClientOptions(options(endpoint)), operation(controller.signal)),
    { kind: 'cancelled' },
  );
  assert.deepEqual(requests, ['Hello']);
});
