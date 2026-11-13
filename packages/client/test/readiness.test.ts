import test from 'node:test';
import assert from 'node:assert/strict';
import { DocsbaseClient } from '../src/index.ts';
import { peer, options, hello, ok, line } from './peer.ts';

const status = {
  schema_version: 1,
  profile: 'website',
  project: { id: 7, name: 'docs', root: '/docs', status: 'indexed' },
  index_generation: 3,
  index_attempt: 4,
  index_health: 'healthy',
  outcome: {
    schema_version: 1,
    attempt_id: 4,
    index_generation: 3,
    canonical_root: '/docs',
    recipe_id: `index-recipe-v1:${'a'.repeat(64)}`,
    scope: 'full_survey',
    stats: { docs: 0, chunks: 0, skipped: 2, removed: 0, errors: 0, warnings: [] },
  },
  active_sync: null,
  latest_sync: null,
  ready: true,
  reason: null,
  restart_required: false,
  notice: null,
};

test('serverStatus binds read-only and dispatches only the versioned fixed-project control tool', async (t) => {
  const requests: string[] = [];
  const endpoint = await peer(t, (request, socket) => {
    requests.push(request.method);
    if (request.method === 'Hello')
      socket.write(
        line({
          ...hello,
          data: { ...hello.data, capabilities: [...hello.data.capabilities, 'server-status-v1'] },
        }),
      );
    else if (request.method === 'BindReadOnly') {
      assert.equal(request.params.project_id, 7);
      socket.write(line(ok));
    } else {
      assert.deepEqual(request, {
        method: 'CallTool',
        params: { name: 'server_status', args: {} },
      });
      socket.write(line({ kind: 'ToolResult', data: { value: status } }));
    }
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  assert.deepEqual(await client.serverStatus(), status);
  assert.deepEqual(requests, ['Hello', 'BindReadOnly', 'CallTool']);
});

test('serverStatus refuses missing capability before tool dispatch', async (t) => {
  const requests: string[] = [];
  const endpoint = await peer(t, (request, socket) => {
    requests.push(request.method);
    if (request.method === 'Hello') socket.write(line(hello));
    else if (request.method === 'BindReadOnly') socket.write(line(ok));
    else assert.fail('unsupported server_status must never dispatch');
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  await assert.rejects(client.serverStatus(), { kind: 'protocol' });
  assert.deepEqual(requests, ['Hello', 'BindReadOnly']);
});

test('serverStatus rejects malformed counts, unrelated project and unsupported DTO schema', async (t) => {
  for (const change of [
    { project: { ...status.project, id: 8 } },
    { schema_version: 2 },
    { outcome: { ...status.outcome, stats: { errors: 0 } } },
    { latest_sync: { job_id: 1, state: 'done', attempt_id: 'wrong', stats: null } },
    { ready: true, reason: 'partial' },
  ]) {
    const endpoint = await peer(t, (request, socket) => {
      if (request.method === 'Hello')
        socket.write(
          line({
            ...hello,
            data: { ...hello.data, capabilities: [...hello.data.capabilities, 'server-status-v1'] },
          }),
        );
      else if (request.method === 'BindReadOnly') socket.write(line(ok));
      else socket.write(line({ kind: 'ToolResult', data: { value: { ...status, ...change } } }));
    });
    const client = new DocsbaseClient(options(endpoint));
    t.after(() => client.close());
    await assert.rejects(client.serverStatus(), { kind: 'protocol' });
  }
});

test('serverStatus preserves typed daemon refusal without replay', async (t) => {
  let calls = 0;
  const endpoint = await peer(t, (request, socket) => {
    if (request.method === 'Hello')
      socket.write(
        line({
          ...hello,
          data: { ...hello.data, capabilities: [...hello.data.capabilities, 'server-status-v1'] },
        }),
      );
    else if (request.method === 'BindReadOnly') socket.write(line(ok));
    else {
      calls++;
      socket.write(
        line({
          kind: 'Error',
          data: {
            code: -32010,
            message: 'fixed-project admission denied',
            instruction: 'use the fixed daemon',
          },
        }),
      );
    }
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  await assert.rejects(client.serverStatus(), {
    kind: 'daemon',
    code: -32010,
    instruction: 'use the fixed daemon',
  });
  assert.equal(calls, 1);
});
