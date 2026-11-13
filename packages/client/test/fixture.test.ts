import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  PROTOCOL_VERSION,
  MAX_FRAME_BYTES,
  SEARCH_CONTEXT,
  PANEL_READ,
  LITERAL_QUERY,
  decodeResponse,
  encodeRequest,
  searchResult,
  toolValue,
} from '../src/protocol.ts';

const fixture = JSON.parse(
  readFileSync(new URL('./fixtures/protocol.json', import.meta.url), 'utf8'),
);
test('Rust-generated protocol identities and request frames match Node transport', () => {
  assert.equal(PROTOCOL_VERSION, fixture.protocolVersion);
  assert.equal(MAX_FRAME_BYTES, fixture.maxFrameBytes);
  assert.deepEqual([SEARCH_CONTEXT, PANEL_READ, LITERAL_QUERY], fixture.capabilities);
  for (const encoded of Object.values(fixture.requests) as string[]) {
    assert.equal(encodeRequest(JSON.parse(encoded)).toString(), encoded);
  }
});
test('Rust-generated Hello/citation/error frames decode without integer loss', () => {
  const hello = decodeResponse(Buffer.from(fixture.responses.hello));
  assert.equal(hello.kind, 'Hello');
  if (hello.kind !== 'Hello') throw new Error('not Hello');
  assert.equal(hello.data.schema_version, fixture.schemaVersion);
  assert.equal(hello.data.build_id, fixture.buildId);
  for (const name of ['hits', 'explained']) {
    const result = searchResult(toolValue(decodeResponse(Buffer.from(fixture.responses[name]))));
    const hits = Array.isArray(result) ? result : result.hits;
    assert.equal(hits[0]?.chunk_id, '18446744073709551615');
    assert.equal(hits[0]?.path, 'guides/привет.md');
  }
  for (const encoded of fixture.errors as string[]) {
    const response = decodeResponse(Buffer.from(encoded));
    assert.equal(response.kind, 'Error');
    if (response.kind !== 'Error') throw new Error('not error');
    assert.throws(() => toolValue(response), { kind: 'daemon', code: response.data.code });
  }
});
