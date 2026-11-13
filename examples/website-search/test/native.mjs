// Cargo invokes the built example with the real SDK and native daemon on each OS.
import assert from 'node:assert/strict';
import http from 'node:http';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { watch, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { resolveWebsiteConfig } from '../dist/config.js';
import { createWebsiteServer } from '../dist/server.js';
import { registeredProject } from '../../../packages/client/test/native-project.ts';
import { daemonEndpoint } from '../../../packages/client/test/native-state.ts';

const [binary, root, expectedBuildId, schema] = process.argv.slice(2);
assert.ok(binary && root && expectedBuildId && schema);
const cache = join(root, 'cache');
const docs = join(root, 'docs');
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
let daemon;
let exited;
let service;
let daemonLog = '';
async function start() {
  mkdirSync(join(cache, 'state'), { recursive: true });
  const events = watch(join(cache, 'state'));
  const startup = new AbortController();
  const signal = AbortSignal.any([startup.signal, AbortSignal.timeout(60000)]);
  daemon = spawn(binary, ['serve', '--project', docs], {
    cwd: root,
    env: process.env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  exited = once(daemon, 'exit');
  daemon.once('error', (error) => startup.abort(error));
  daemon.once('exit', () => startup.abort(new Error('daemon exited before startup')));
  for (const stream of [daemon.stdout, daemon.stderr])
    stream.on('data', (bytes) => {
      daemonLog = (daemonLog + bytes.toString()).slice(-65536);
    });
  try {
    for (;;) {
      let state;
      try {
        state = JSON.parse(readFileSync(join(cache, 'state', 'daemon.json'), 'utf8'));
      } catch {}
      if (state?.pid === daemon.pid) return state;
      await once(events, 'change', { signal });
    }
  } finally {
    events.close();
  }
}
async function stop() {
  await run(['daemon', 'stop']);
  await exited;
  daemon = undefined;
}
function request(port, path = '/search', query = 'originalwidget', method = 'POST') {
  let req;
  const result = new Promise((resolve, reject) => {
    req = http.request(
      {
        host: '127.0.0.1',
        port,
        path,
        method,
        agent: false,
        headers: { 'content-type': 'application/json' },
      },
      (res) => {
        let body = '';
        res.setEncoding('utf8');
        res.on('data', (chunk) => {
          body += chunk;
        });
        res.on('error', reject);
        res.on('end', () => resolve({ status: res.statusCode, value: JSON.parse(body) }));
      },
    );
    req.on('error', reject);
    req.end(method === 'POST' ? JSON.stringify({ query, options: { snippet: true } }) : undefined);
  });
  void result.catch(() => {});
  return { req, result };
}
try {
  mkdirSync(docs);
  mkdirSync(join(root, 'config'));
  writeFileSync(join(root, 'config', 'config.toml'), 'auto_index=false\nhybrid=false\n');
  writeFileSync(join(docs, 'guide.md'), '# Guide\n\noriginalwidget stays local.\n');
  writeFileSync(join(docs, 'obsolete.md'), '# Old\n\nobsoletewidget\n');
  await run(['index', docs]);
  const project = registeredProject(JSON.parse(await run(['status'])).projects, docs);
  const endpoint = daemonEndpoint(await start());
  const config = resolveWebsiteConfig({
    http: {
      host: '127.0.0.1',
      port: 0,
      maxConnections: 16,
      searchActiveRequests: 1,
      searchPendingRequests: 1,
      controlActiveRequests: 1,
      controlPendingRequests: 1,
      maxHeaderBytes: 8192,
      maxHeadersCount: 32,
      maxBodyBytes: 4096,
      maxQueryBytes: 1024,
      maxResultBytes: 65536,
      maxHits: 10,
      requestTimeoutMs: 30000,
      headersTimeoutMs: 30000,
      headerCheckIntervalMs: 1000,
      idleTimeoutMs: 30000,
      keepAliveTimeoutMs: 1000,
      maxRequestsPerSocket: 20,
    },
    client: {
      endpoint,
      projectId: project.id,
      expectedBuildId,
      expectedSchemaVersion: Number(schema),
      settings: {
        searchConnections: 1,
        controlConnections: 1,
        queueLimit: 1,
        requestTimeoutMs: 30000,
      },
    },
  });
  service = createWebsiteServer(config);
  service.server.listen(config.http.port, config.http.host);
  await once(service.server, 'listening');
  const port = service.server.address().port;
  assert.equal((await request(port, '/ready', '', 'GET').result).value.ready, true);
  const first = await request(port).result;
  assert.equal(first.status, 200);
  assert.equal(first.value.length, 1);
  assert.match(first.value[0].snippet, /originalwidget/);
  assert.match(first.value[0].chunk_id, /^[0-9]+$/);

  writeFileSync(join(docs, 'guide.md'), '# Guide\n\nreplacementwidget\n');
  writeFileSync(join(docs, 'added.md'), '# Added\n\naddedwidget\n');
  rmSync(join(docs, 'obsolete.md'));
  assert.equal((await request(port, '/search', 'replacementwidget').result).value.length, 0);
  await run(['sync', docs]);
  assert.equal((await request(port, '/search', 'replacementwidget').result).value.length, 1);
  assert.equal((await request(port, '/search', 'addedwidget').result).value.length, 1);
  assert.equal((await request(port, '/search', 'obsoletewidget').result).value.length, 0);
  assert.equal((await request(port, '/ready', '', 'GET').result).status, 200);
  const status = JSON.parse(await run(['status']));
  assert.equal(status.watchers, 0);

  writeFileSync(join(docs, '.docsbase.toml'), 'max_file_size=1\n');
  await run(['sync', docs]);
  const partial = await request(port, '/ready', '', 'GET').result;
  assert.equal(partial.status, 503);
  assert.equal(partial.value.ready, false);
  assert.equal(partial.value.reason, 'partial');
  writeFileSync(join(docs, '.docsbase.toml'), '');
  await run(['sync', docs]);
  assert.equal((await request(port, '/ready', '', 'GET').result).value.ready, true);

  // Real HTTP upload provides a deterministic gate without a daemon delay knob.
  // Admission happens before body buffering/SDK work; independent readiness
  // still traverses the actual SDK/native IPC while the search lane is full.
  const admitted = once(service.server, 'request');
  const held = http.request({
    host: '127.0.0.1',
    port,
    path: '/search',
    method: 'POST',
    agent: false,
    headers: { 'content-type': 'application/json', 'content-length': 100 },
  });
  held.on('error', () => {});
  held.write('{');
  const [, heldResponse] = await admitted;
  const queued = once(service.server, 'request');
  const removed = request(port, '/search', 'removedwidget');
  const [, queuedResponse] = await queued;
  assert.equal((await request(port, '/search', 'overflowwidget').result).status, 503);
  assert.equal((await request(port, '/ready', '', 'GET').result).status, 200);
  const queueClosed = once(queuedResponse, 'close');
  removed.req.destroy();
  await assert.rejects(removed.result);
  await queueClosed;
  const bodyClosed = once(heldResponse, 'close');
  held.destroy();
  await bodyClosed;
  assert.equal((await request(port, '/search', 'replacementwidget').result).status, 200);

  await stop();
  assert.equal((await request(port, '/search', 'replacementwidget').result).status, 503);
  assert.equal((await request(port, '/ready', '', 'GET').result).status, 503);
  assert.equal(daemonEndpoint(await start()), endpoint);
  // Already-closed idle connections can be retired before dispatch. Both calls
  // failed while the daemon was absent; these later calls negotiate afresh.
  assert.equal((await request(port, '/search', 'replacementwidget').result).status, 200);
  assert.equal((await request(port, '/ready', '', 'GET').result).status, 200);
  assert.equal(existsSync(join(cache, 'vectors.db')), false);
  assert.equal(existsSync(join(cache, 'models')), false);
  await service.close();
  service = undefined;
  await stop();
  process.stdout.write(
    JSON.stringify({
      httpNativeIpc: true,
      explicitUpdates: true,
      partialRecovery: true,
      boundedAdmission: true,
      cancelledRequests: true,
      reconnected: true,
      watchersOff: true,
    }) + '\n',
  );
} catch (error) {
  console.error(daemonLog);
  throw error;
} finally {
  if (service) await service.close();
  if (daemon) {
    try {
      await stop();
    } catch {
      daemon.kill();
      await exited;
    }
  }
}
