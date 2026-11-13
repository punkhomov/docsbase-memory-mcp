import assert from 'node:assert/strict';
import test from 'node:test';
import { daemonEndpoint } from './native-state.ts';

test('native fixture consumes the recorded Unix daemon endpoint', () => {
  for (const platform of ['linux', 'darwin'] as const) {
    const socket = '/canonical-cache/state/daemon.sock';
    assert.equal(daemonEndpoint({ socket }, platform), socket);
  }
});

test('native fixture qualifies the bare pipe name serialized by Rust', () => {
  assert.equal(
    daemonEndpoint({ socket: 'docsbase-recorded-by-daemon' }, 'win32'),
    '\\\\.\\pipe\\docsbase-recorded-by-daemon',
  );
});

test('native fixture fails clearly when ready daemon state lacks an endpoint', () => {
  for (const socket of [undefined, null, '', 79]) {
    assert.throws(() => daemonEndpoint({ socket }), {
      message: 'daemon state must include a nonempty socket',
    });
  }
});
