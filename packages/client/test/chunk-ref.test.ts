import test from 'node:test';
import assert from 'node:assert/strict';
import { DocsbaseClient } from '../src/index.ts';
import { peer, options, hello, ok, line } from './peer.ts';

const reference = `v1.${'1'.repeat(96)}`;
const location = { chunk_id: 3, path: 'guide.md', heading_path: 'Guide', lines: [1, 2] };
const result = (value: unknown) => line({ kind: 'ToolResult', data: { value } });

test('scoped search and pinned neighbors exchange an opaque chunk reference', async (t) => {
  const endpoint = await peer(t, (request, socket) => {
    if (request.method === 'Hello') {
      socket.write(
        line({
          kind: 'Hello',
          data: { ...hello.data, capabilities: [...hello.data.capabilities, 'chunk-ref-v1'] },
        }),
      );
    } else if (request.method === 'BindReadOnly') {
      socket.write(line(ok));
    } else if (request.method === 'Search') {
      assert.equal(request.params.args.chunk_ref, true);
      socket.write(result([{ ...location, score: 1, chunk_ref: reference }]));
    } else {
      assert.deepEqual(request.params, {
        name: 'read_neighbors_at',
        args: { chunk_ref: reference, before: 0 },
      });
      socket.write(result([{ ...location, seq: 0, kind: 'paragraph', text: 'snapshot text' }]));
    }
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  const hits = await client.search('snapshot', { chunk_ref: true });
  assert.ok(Array.isArray(hits));
  assert.equal(hits[0]?.chunk_ref, reference);
  const neighbors = await client.readNeighborsAt(reference, { before: 0 });
  assert.equal(neighbors[0]?.chunk_id, '3');
  assert.equal(neighbors[0]?.text, 'snapshot text');
});

test('a peer without chunk-ref-v1 receives no opt-in search or pinned read', async (t) => {
  const endpoint = await peer(t, (request, socket) => {
    if (request.method === 'Hello') socket.write(line(hello));
    else if (request.method === 'BindReadOnly') socket.write(line(ok));
    else assert.fail('chunk reference request was sent to an unsupported peer');
  });
  const client = new DocsbaseClient(options(endpoint));
  t.after(() => client.close());
  await assert.rejects(client.search('snapshot', { chunk_ref: true }), { kind: 'protocol' });
  await assert.rejects(client.readNeighborsAt(reference), { kind: 'protocol' });
  await assert.rejects(client.readNeighborsAt(reference + '\n'), { kind: 'validation' });
});
