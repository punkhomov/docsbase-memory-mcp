// Invoked by Cargo's native integration test. The package self-reference checks exports.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { watch, mkdirSync, writeFileSync, readFileSync, existsSync, symlinkSync } from 'node:fs';
import { join } from 'node:path';
import { once } from 'node:events';
import { DocsbaseClient } from '@docsbase/client';
import { registeredProject } from './native-project.ts';
import { daemonEndpoint } from './native-state.ts';

const [binary, root, endpoint, expectedBuildId, rawSchema] = process.argv.slice(2);
assert.ok(binary && root && endpoint && expectedBuildId && rawSchema);
const cache = join(root, 'cache');
const config = {
  endpoint,
  projectId: 1,
  expectedBuildId,
  expectedSchemaVersion: Number(rawSchema),
  settings: { searchConnections: 3, controlConnections: 1, queueLimit: 8, requestTimeoutMs: 30000 },
};
const exec = promisify(execFile);
const run = async (args) =>
  (
    await exec(binary, args, {
      cwd: root,
      env: process.env,
      timeout: 60000,
      maxBuffer: 8 * 1024 * 1024,
    })
  ).stdout;
const clients = [];
const make = (overrides) => {
  const client = new DocsbaseClient({ ...config, ...overrides });
  clients.push(client);
  return client;
};
let daemon;
let daemonExit;
let daemonLog = '';
async function start() {
  mkdirSync(join(cache, 'state'), { recursive: true });
  // Subscribe before spawn, then probe before waiting: no missed readiness edge.
  const events = watch(join(cache, 'state'));
  const stopped = new AbortController();
  const signal = AbortSignal.any([stopped.signal, AbortSignal.timeout(60000)]);
  daemon = spawn(binary, ['serve', '--grace-ms', '60000'], {
    cwd: root,
    env: process.env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  daemonExit = once(daemon, 'exit');
  daemon.once('error', (error) => stopped.abort(error));
  daemon.once('exit', () => stopped.abort(new Error('daemon exited before ready')));
  for (const stream of [daemon.stdout, daemon.stderr]) {
    stream.on('data', (bytes) => {
      daemonLog = (daemonLog + bytes.toString()).slice(-65536);
    });
  }
  try {
    for (;;) {
      let state;
      try {
        state = JSON.parse(readFileSync(join(cache, 'state', 'daemon.json'), 'utf8'));
      } catch {}
      if (state?.pid === daemon.pid) return state;
      await once(events, 'change', { signal });
    }
  } catch (error) {
    throw new Error(`daemon startup failed: ${daemonLog}`, { cause: error });
  } finally {
    events.close();
  }
}
async function stop() {
  await run(['daemon', 'stop']);
  if (daemonExit) await daemonExit;
  daemon = undefined;
}
try {
  const absent = make();
  await assert.rejects(absent.status(), { kind: 'transport' });
  absent.close();
  assert.equal(existsSync(cache), false, 'client must not create cache or autostart');
  const canonicalAlpha = join(root, 'alpha');
  mkdirSync(canonicalAlpha);
  const alpha = join(root, 'alpha-alias'),
    beta = join(root, 'beta');
  symlinkSync(canonicalAlpha, alpha, 'junction');
  for (const dir of [alpha, beta, join(alpha, 'guides'), join(root, 'config')])
    mkdirSync(dir, { recursive: true });
  for (const dir of [alpha, beta]) await exec('git', ['init', dir], { timeout: 60000 });
  writeFileSync(join(root, 'config', 'config.toml'), 'auto_index = false\nhybrid = false\n');
  writeFileSync(
    join(alpha, 'guides', 'install.md'),
    '# Widgets installation\n\nInstall widgets for local authentication.\n\n## Configuration\n\nWidgets configuration stays local.\n',
  );
  writeFileSync(
    join(alpha, 'guides', 'routing.md'),
    '# Widgets routing\n\nWidgets routes use local configuration.\n',
  );
  writeFileSync(join(beta, 'private.md'), '# Bananas\n\nBananas belong exclusively to beta.\n');
  await run(['index', alpha]);
  await run(['index', beta]);
  const direct = JSON.parse(await run(['status']));
  const project = registeredProject(direct.projects, alpha);
  const other = registeredProject(direct.projects, beta);
  assert.notEqual(project.id, other.id);
  config.projectId = project.id;
  // The supplied endpoint was computed while cache was absent. Use the actual
  // daemon's published endpoint now that registration has created the cache.
  config.endpoint = daemonEndpoint(await start());
  const client = make();
  const snapshot = await client.status();
  assert.equal(snapshot.project.id, project.id);
  assert.equal(snapshot.project.docs, 2);
  assert.ok(snapshot.project.index_generation > 0);
  assert.equal('projects' in snapshot, false);
  const hits = await client.search('widgets', {
    query_mode: 'literal',
    snippet: true,
    dedupe: 'path',
    n_per_doc: 1,
    limit: 10,
  });
  assert.ok(Array.isArray(hits));
  assert.equal(hits.length, 2);
  assert.equal(new Set(hits.map((hit) => hit.path)).size, 2);
  for (const hit of hits) {
    assert.match(hit.chunk_id, /^[0-9]+$/);
    assert.match(hit.snippet, /widgets/i);
    assert.equal(hit.lines.length, 2);
    assert.equal(typeof hit.score, 'number');
  }
  const filtered = await client.search('widgets', {
    path_prefix: 'guides/install',
    dedupe: 'path',
    n_per_doc: 1,
  });
  assert.ok(Array.isArray(filtered));
  assert.equal(filtered.length, 1);
  assert.match(filtered[0].path, /install\.md$/);
  const explained = await client.search('widgets', { explain: true });
  assert.equal(Array.isArray(explained), false);
  assert.ok(explained.hits.length > 0);
  assert.equal(explained.meta.index_generation, snapshot.project.index_generation);
  assert.deepEqual(await client.search('bananas'), []);
  const betaClient = make({ projectId: other.id });
  assert.ok((await betaClient.search('bananas')).length > 0);
  const unknown = make({ projectId: Number.MAX_SAFE_INTEGER });
  await assert.rejects(unknown.search('widgets'), { kind: 'daemon', code: -32012 });
  unknown.close();
  betaClient.close();
  const wrongBuild = make({ expectedBuildId: 'incompatible build' });
  await assert.rejects(wrongBuild.status(), { kind: 'daemon', code: -32010 });
  wrongBuild.close();
  // All calls are admitted synchronously before native I/O can release slots:
  // 3 active + 8 pending, then exactly one local overload.
  const batch = await Promise.allSettled(
    Array.from({ length: 12 }, () => client.search('widgets')),
  );
  const accepted = batch.filter((entry) => entry.status === 'fulfilled');
  const rejected = batch.filter((entry) => entry.status === 'rejected');
  assert.equal(accepted.length, 11);
  assert.equal(rejected.length, 1);
  assert.equal(rejected[0].reason.kind, 'overloaded');
  const firstPid = daemon.pid;
  await stop();
  await assert.rejects(client.search('widgets'), { kind: 'transport' });
  assert.equal(existsSync(join(cache, 'state', 'daemon.json')), false);
  assert.equal(
    daemonEndpoint(await start()),
    config.endpoint,
    'daemon endpoint must remain stable across restart for the existing client',
  );
  assert.notEqual(daemon.pid, firstPid);
  assert.equal((await client.status()).project.index_generation, snapshot.project.index_generation);
  assert.equal((await client.search('widgets')).length > 0, true);
  client.close();
  await assert.rejects(client.status(), { kind: 'closed' });
  const observer = make();
  assert.equal((await observer.status()).project.id, project.id);
  observer.close();
  assert.equal(existsSync(join(cache, 'vectors.db')), false);
  assert.equal(existsSync(join(cache, 'models')), false);
  await stop();
  process.stdout.write(
    JSON.stringify({
      nativeIpc: true,
      searchesAccepted: accepted.length,
      overloaded: rejected.length,
      reconnected: true,
      providerArtifacts: false,
    }) + '\n',
  );
} catch (error) {
  console.error(
    JSON.stringify({
      suppliedEndpoint: endpoint.slice(0, 512),
      selectedEndpoint: config.endpoint.slice(0, 512),
    }),
  );
  console.error(daemonLog);
  throw error;
} finally {
  for (const client of clients) client.close();
  if (daemon) {
    try {
      await stop();
    } catch {
      daemon.kill();
      await daemonExit;
    }
  }
}
