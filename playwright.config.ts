import { defineConfig, devices } from '@playwright/test';

const isCi = Boolean(process.env['CI']);

/**
 * E2E tests drive the React application in a real Chromium against the Vite dev server, with the
 * Tauri IPC layer mocked (see tests/e2e/fixtures). Smoke tests against the packaged Windows binary
 * are driven separately by tauri-driver and are not part of this config.
 */
export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: true,
  forbidOnly: isCi,
  retries: isCi ? 2 : 0,
  reporter: isCi ? 'github' : 'list',
  use: {
    baseURL: 'http://localhost:1420',
    trace: 'on-first-retry',
    viewport: { width: 1280, height: 800 },
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'pnpm dev',
    url: 'http://localhost:1420',
    reuseExistingServer: !isCi,
    timeout: 60_000,
  },
});
