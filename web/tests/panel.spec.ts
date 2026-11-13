import { test, expect, type TestInfo } from '@playwright/test';
import { spawn, execFileSync, type ChildProcess } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
let root: string,
  base: string,
  token: string,
  child: ChildProcess,
  env: NodeJS.ProcessEnv,
  cookies: any[];
const binary = path.resolve(process.env.DOCSBASE_WEB_BINARY || '../target/debug/docsbase');
let daemonChild: ChildProcess | undefined;
let daemonLog = '';
let webLog = '';
let browserLogs: string[] = [];
async function attachDiagnostics(testInfo: TestInfo) {
  const safe = (value: string) =>
    value
      .replace(/#bootstrap=[a-f0-9]+/g, '#bootstrap=[redacted]')
      .replaceAll('DO-NOT-EXPOSE-THIS-SECRET', '[redacted]');
  await testInfo.attach('restarted-daemon-log', {
    body: safe(daemonLog),
    contentType: 'text/plain',
  });
  await testInfo.attach('web-log', { body: safe(webLog), contentType: 'text/plain' });
  await testInfo.attach('browser-console', {
    body: safe(browserLogs.join('\n')),
    contentType: 'text/plain',
  });
  if (root) {
    const log = path.join(root, 'cache', 'logs', 'daemon.log');
    if (existsSync(log))
      await testInfo.attach('daemon-log', {
        body: safe(readFileSync(log, 'utf8')),
        contentType: 'text/plain',
      });
    await testInfo.attach('fixture', {
      body: JSON.stringify(
        {
          root,
          binary,
          base,
          projects: ['alpha', 'beta'],
          poll_interval_ms: 150,
          worker: testInfo.workerIndex,
        },
        null,
        2,
      ),
      contentType: 'application/json',
    });
  }
}
test.beforeAll(async ({ browser }, testInfo) => {
  try {
    root = mkdtempSync(path.join(tmpdir(), 'docsbase-web-'));
    const a = path.join(root, 'alpha'),
      b = path.join(root, 'beta');
    for (const dir of [a, b, path.join(root, 'config')]) mkdirSync(dir);
    for (const dir of [a, b]) execFileSync('git', ['init', dir], { stdio: 'pipe' });
    const nested = path.join(a, 'documentation-' + 'long-path-'.repeat(15));
    mkdirSync(nested);
    writeFileSync(
      path.join(nested, 'guide.md'),
      '# Cancellation guide with a deliberately long heading that remains readable on narrow screens\n\nCancellation closes the transport. Widgets are indexed locally.\n\n<script>window.injected = true</script>\n',
    );
    writeFileSync(
      path.join(b, 'second.md'),
      '# Second workspace\n\nBananas belong to the second project.\n',
    );
    writeFileSync(path.join(root, 'config', 'web.toml'), 'poll_interval_ms = 150\n');
    writeFileSync(
      path.join(root, 'config', 'config.toml'),
      'ignores = ["' + 'a-long-safe-ignore-pattern-'.repeat(20) + '"]\n',
    );
    env = {
      ...process.env,
      DOCSBASE_CACHE_DIR: path.join(root, 'cache'),
      DOCSBASE_CONFIG_DIR: path.join(root, 'config'),
      DOCSBASE_DATA_DIR: path.join(root, 'data'),
      OPENROUTER_API_KEY: 'DO-NOT-EXPOSE-THIS-SECRET',
    };
    for (const dir of [a, b])
      execFileSync(binary, ['index', dir], { env, timeout: 60000, stdio: 'pipe' });
    child = spawn(binary, ['web'], { cwd: a, env, stdio: ['ignore', 'pipe', 'pipe'] });
    const launch = await new Promise<string>((resolve, reject) => {
      let output = '';
      const timer = setTimeout(() => reject(new Error('Web startup watchdog')), 30000);
      child.stdout!.on('data', (chunk) => {
        output += chunk;
        const found = output.match(/http:\/\/127\.0\.0\.1:\d+\/#bootstrap=[a-f0-9]+/);
        if (found) {
          clearTimeout(timer);
          resolve(found[0]);
        }
      });
      child.stderr!.on('data', (chunk) => {
        webLog += chunk;
      });
      child.once('exit', (code) => {
        clearTimeout(timer);
        reject(new Error(`Web exited ${code}: ${webLog}`));
      });
    });
    base = launch.split('/#')[0];
    token = launch.split('bootstrap=')[1];
    const context = await browser.newContext();
    const page = await context.newPage();
    await page.goto(launch);
    await expect(page.locator('.connection').filter({ hasText: 'Connected' })).toBeVisible();
    cookies = await context.cookies();
    await context.close();
  } catch (error) {
    await attachDiagnostics(testInfo);
    throw error;
  }
});
test.beforeEach(async ({ context, page }) => {
  await context.addCookies(cookies);
  browserLogs = [];
  page.on('pageerror', (e) => browserLogs.push(e.message));
  page.on('console', (m) => {
    if (m.type() === 'error') browserLogs.push(m.text());
  });
});
test.afterEach(async ({}, testInfo) => {
  await attachDiagnostics(testInfo);
});
test.afterAll(() => {
  child?.kill('SIGINT');
  if (env)
    try {
      execFileSync(binary, ['daemon', 'stop'], { env, timeout: 10000 });
    } catch {}
  daemonChild?.kill();
  if (root) rmSync(root, { recursive: true, force: true });
});

test('projects, syntax and literal search, safe text, configuration and narrow layout', async ({
  page,
}, testInfo) => {
  const external: string[] = [];
  page.on('request', (r) => {
    if (!r.url().startsWith(base)) external.push(r.url());
  });
  await page.goto(base);
  await expect(page.getByRole('heading', { name: 'Projects', exact: true })).toBeVisible();
  await expect(page.locator('.project-card')).toHaveCount(2);
  await page.screenshot({ path: testInfo.outputPath('projects.png'), fullPage: true });
  await page.getByRole('button', { name: 'Search this project' }).click();
  await page.getByLabel('Search alpha').fill('widgets');
  await page.getByRole('button', { name: 'Search', exact: true }).last().click();
  await expect(page.locator('.result')).toHaveCount(1);
  await expect(page.locator('.result')).toContainText('guide.md');
  await expect(page.locator('.result')).toContainText('<script>');
  expect(await page.evaluate(() => (window as any).injected)).toBeUndefined();
  await page.getByLabel('Interpretation').selectOption('literal');
  await page.getByLabel('Search alpha').fill('Cancellation: transport');
  await page.getByRole('button', { name: 'Search', exact: true }).last().click();
  await expect(page.locator('.result')).toHaveCount(1);
  await page.screenshot({ path: testInfo.outputPath('search.png'), fullPage: true });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath('mobile.png'), fullPage: true });
  await page.getByRole('button', { name: 'Configuration', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Index', exact: true })).toBeVisible();
  expect(await page.locator('body').textContent()).not.toContain('DO-NOT-EXPOSE');
  await page.screenshot({ path: testInfo.outputPath('configuration.png'), fullPage: true });
  expect(external).toEqual([]);
  expect(browserLogs).toEqual([]);
});
test('empty and invalid queries, project switching and keyboard submission', async ({
  page,
}, testInfo) => {
  await page.goto(base);
  await page.getByRole('button', { name: 'Search this project' }).click();
  const query = page.getByLabel('Search alpha');
  await query.fill('zzzzdoesnotexist');
  await query.press('Enter');
  await expect(page.getByRole('heading', { name: 'No matches' })).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('empty.png'), fullPage: true });
  await query.fill('"');
  await query.press('Enter');
  await expect(page.getByRole('alert')).toContainText('Invalid');
  await page.screenshot({ path: testInfo.outputPath('error.png'), fullPage: true });
  await page.getByLabel('PROJECT', { exact: true }).selectOption({ label: 'beta' });
  await page.getByLabel('Search beta').fill('bananas');
  await page.getByLabel('Search beta').press('Enter');
  await expect(page.locator('.result')).toContainText('second.md');
});
test('HTTP session, Host, Origin, body limit, traversal and secret exclusions', async ({
  request,
}) => {
  const cookie = cookies.map((c) => `${c.name}=${c.value}`).join('; ');
  expect((await request.get(`${base}/api/v1/projects`)).status()).toBe(401);
  expect(
    (
      await request.get(`${base}/api/v1/projects`, {
        headers: { Cookie: cookie, Host: 'evil.example' },
      })
    ).status(),
  ).toBe(403);
  expect(
    (
      await request.get(`${base}/api/v1/projects`, {
        headers: { Cookie: cookie, Origin: 'https://evil.example' },
      })
    ).status(),
  ).toBe(403);
  expect(
    (
      await request.post(`${base}/api/v1/session`, { headers: { Origin: base }, data: { token } })
    ).status(),
  ).toBe(401);
  const list = await (
    await request.get(`${base}/api/v1/projects`, { headers: { Cookie: cookie } })
  ).json();
  const id = list[0].id;
  const config = await request.get(`${base}/api/v1/projects/${id}/config`, {
    headers: { Cookie: cookie },
  });
  expect(config.status()).toBe(200);
  expect(await config.text()).not.toContain('DO-NOT-EXPOSE');
  expect(
    (
      await request.post(`${base}/api/v1/projects/${id}/search`, {
        headers: { Cookie: cookie, Origin: base },
        data: { query: 'x'.repeat(70000) },
      })
    ).status(),
  ).toBe(413);
  expect(
    (
      await request.get(`${base}/api/v1/projects/999999/status`, { headers: { Cookie: cookie } })
    ).status(),
  ).toBe(404);
  expect((await request.get(`${base}/%2e%2e/Cargo.toml`)).status()).toBe(404);
  expect(config.headers()['cache-control']).toBe('no-store');
  expect(cookies[0].httpOnly).toBe(true);
  expect(cookies[0].sameSite).toBe('Strict');
});
test('cancel, replace and switch project cannot publish a late real search response', async ({
  page,
}, testInfo) => {
  await page.goto(base);
  await page.getByRole('button', { name: 'Search this project' }).click();
  let release!: () => void;
  let reached!: () => void;
  let gate = new Promise<void>((r) => (release = r));
  let entered = new Promise<void>((r) => (reached = r));
  await page.route('**/search', async (route) => {
    const response = await route.fetch(); // Execute the real daemon search; only delivery is delayed.
    reached();
    await gate;
    await route.fulfill({ response }).catch(() => {});
  });
  await page.getByLabel('Search alpha').fill('widgets');
  await page.getByLabel('Search alpha').press('Enter');
  await entered;
  await page.screenshot({ path: testInfo.outputPath('loading.png'), fullPage: true });
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(page.getByText('Search cancelled. No automatic retry.')).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath('cancelled.png'), fullPage: true });
  release();
  await expect(page.locator('.result')).toHaveCount(0);
  gate = new Promise<void>((r) => (release = r));
  entered = new Promise<void>((r) => (reached = r));
  await page.getByLabel('Search alpha').press('Enter');
  await entered;
  await page.getByLabel('PROJECT', { exact: true }).selectOption({ label: 'beta' });
  release();
  await expect(page.getByLabel('Search beta')).toBeVisible();
  await expect(page.locator('.result')).toHaveCount(0);
  await page.unroute('**/search');
  await page.getByLabel('Search beta').fill('bananas');
  await page.getByLabel('Search beta').press('Enter');
  await expect(page.locator('.result')).toContainText('second.md');
});
test('project read failure cannot overlap its unfinished sibling', async ({ page }) => {
  await page.goto(base);
  await expect(page.locator('.project-card')).toHaveCount(2);
  const beta = await page
    .getByLabel('PROJECT', { exact: true })
    .locator('option')
    .filter({ hasText: 'beta' })
    .getAttribute('value');
  let release!: () => void;
  let reached!: () => void;
  let observed!: () => void;
  let configs = 0;
  let polls = 0;
  const gate = new Promise<void>((r) => (release = r));
  const entered = new Promise<void>((r) => (reached = r));
  const polling = new Promise<void>((r) => (observed = r));
  page.on('request', (request) => {
    if (request.url().endsWith('/session') && ++polls === 2) observed();
  });
  await page.route(`**/projects/${beta}/config`, async (route) => {
    configs++;
    const response = await route.fetch();
    reached();
    await gate;
    await route.fulfill({ response }).catch(() => {});
  });
  await page.route(`**/projects/${beta}/status`, (route) => route.abort('connectionfailed'));
  await page.getByLabel('PROJECT', { exact: true }).selectOption({ label: 'beta' });
  await entered;
  await polling;
  expect(configs).toBe(1);
  release();
  await page.unroute(`**/projects/${beta}/config`);
  await page.unroute(`**/projects/${beta}/status`);
  await expect(page.locator('.connection').filter({ hasText: 'Connected' })).toBeVisible();
});
test('initial connection failure recovers with one initialization', async ({ page }) => {
  let first = true;
  let release!: () => void;
  let reached!: () => void;
  let calls = 0;
  const gate = new Promise<void>((r) => (release = r));
  const entered = new Promise<void>((r) => (reached = r));
  await page.route('**/api/v1/session', async (route) => {
    calls++;
    if (first) {
      first = false;
      await route.abort('connectionfailed');
      return;
    }
    const response = await route.fetch();
    reached();
    await gate;
    await route.fulfill({ response });
  });
  await page.goto(base);
  await expect(page.getByRole('heading', { name: 'Connection unavailable' })).toBeVisible();
  await page.getByRole('button', { name: 'Retry connection' }).click();
  await entered;
  await page.getByRole('button', { name: 'Retry connection' }).click();
  expect(calls).toBe(2);
  release();
  await expect(page.locator('.connection').filter({ hasText: 'Connected' })).toBeVisible();
  await expect(page.locator('.project-card')).toHaveCount(2);
});
test('daemon loss and recovery are visible; polling never retries searches', async ({
  page,
}, testInfo) => {
  await page.goto(base);
  await expect(page.locator('.connection').filter({ hasText: 'Connected' })).toBeVisible();
  let searches = 0;
  page.on('request', (r) => {
    if (r.url().endsWith('/search')) searches++;
  });
  execFileSync(binary, ['daemon', 'stop'], { env, timeout: 10000 });
  await expect(
    page.locator('.connection').filter({ hasText: 'Disconnected · last known data' }),
  ).toBeVisible();
  expect(searches).toBe(0);
  await page.screenshot({ path: testInfo.outputPath('disconnected.png'), fullPage: true });
  daemonChild = spawn(binary, ['serve'], { cwd: root, env, stdio: ['ignore', 'pipe', 'pipe'] });
  daemonChild.stdout!.on('data', (chunk) => (daemonLog += chunk));
  daemonChild.stderr!.on('data', (chunk) => (daemonLog += chunk));
  daemonChild.once('exit', (code) => (daemonLog += `Daemon exit ${code}\n`));
  await expect(page.locator('.connection').filter({ hasText: 'Connected' })).toBeVisible();
  await page.getByRole('button', { name: 'Configuration', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Index', exact: true })).toBeVisible();
  await expect(page.getByRole('alert')).toHaveCount(0);
  expect(searches).toBe(0);
});
