import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, toNamespacedPath } from 'node:path';
import test from 'node:test';
import { registeredProject } from './native-project.ts';

function directories(t: test.TestContext) {
  const root = mkdtempSync(join(tmpdir(), 'docsbase-native-project-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const alpha = join(root, 'alpha');
  const beta = join(root, 'beta');
  const alias = join(root, 'alpha-alias');
  mkdirSync(alpha);
  mkdirSync(beta);
  symlinkSync(alpha, alias, 'junction');
  return { root, alpha, beta, alias };
}

test('native fixture resolves alias and canonical directory to the registered ID', (t) => {
  const { alpha, beta, alias } = directories(t);
  const project = { id: 79, root: realpathSync(alpha), name: 'alpha' };
  const other = { id: 141, root: beta, name: 'alpha' };
  assert.equal(registeredProject([other, project], alias), project);
  assert.equal(registeredProject([project, other], alpha), project);
  assert.equal(registeredProject([project, other], beta), other);
  // Uses the host OS API: on Windows this is a real verbatim directory path.
  assert.equal(registeredProject([{ ...project, root: toNamespacedPath(alpha) }], alias).id, 79);
});

test('native fixture rejects a same-name project at another directory', (t) => {
  const { alpha, beta } = directories(t);
  assert.throws(() => registeredProject([{ id: 141, root: beta, name: 'alpha' }], alpha), {
    message: /registered project lookup failed: matches=0/,
  });
});

test('native fixture rejects ambiguous rows for the same directory', (t) => {
  const { alpha, alias } = directories(t);
  assert.throws(
    () =>
      registeredProject(
        [
          { id: 79, root: alpha },
          { id: 141, root: alias },
        ],
        alpha,
      ),
    { message: /registered project lookup failed: matches=2/ },
  );
});

test('native fixture rejects unreadable candidates even when one directory matches', (t) => {
  const { alpha, root } = directories(t);
  const file = join(root, 'file');
  writeFileSync(file, 'not a project directory');
  assert.throws(
    () =>
      registeredProject(
        [
          { id: 79, root: alpha },
          { id: 141, root: file },
        ],
        alpha,
      ),
    { message: /matches=1, unreadable=1.*ENOTDIR/ },
  );
  assert.throws(() => registeredProject([{ id: 79, root: alpha }], join(root, 'missing')), {
    message: /matches=0.*expected=.*ENOENT/,
  });
});

test('native fixture lookup failures include bounded directory and candidate diagnostics', (t) => {
  const { alpha, beta, root } = directories(t);
  const missing = join(root, 'missing');
  const projects = [
    { id: 141, root: beta },
    { id: 142, root: missing },
    ...Array.from({ length: 50 }, (_, id) => ({ id: id + 200, root: 'x'.repeat(10000) })),
  ];
  assert.throws(
    () => registeredProject(projects, alpha),
    (error: unknown) => {
      assert.ok(error instanceof Error);
      assert.match(error.message, /registered project lookup failed: matches=0/);
      assert.match(error.message, /dev=/);
      assert.match(error.message, /ino=/);
      assert.match(error.message, /141/);
      assert.match(error.message, /ENOENT/);
      assert.match(error.message, /omitted/);
      assert.ok(error.message.length < 5000, `diagnostic has ${error.message.length} chars`);
      return true;
    },
  );
});
