import { defineConfig } from '@playwright/test';
export default defineConfig({ testDir: './tests', testMatch: '**/*.spec.ts', workers: 1, retries: 0, timeout: 60000, use: { headless: true, launchOptions: { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE }, screenshot: 'only-on-failure', trace: 'retain-on-failure' }, reporter: [['list'], ['html', { open: 'never' }]] });
