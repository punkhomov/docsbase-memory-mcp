import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Latest } from '../src/shared/api/latest.js';
test('replacement and project changes abort and reject late results', () => {
  const latest = new Latest(); const first = latest.start(); const second = latest.start();
  assert.equal(first.signal.aborted, true); assert.equal(latest.current(first.generation), false);
  assert.equal(latest.current(second.generation), true); latest.cancel();
  assert.equal(second.signal.aborted, true); assert.equal(latest.current(second.generation), false);
});
