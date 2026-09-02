import { defineConfig, devices } from '@playwright/test';

/**
 * E2E tests run the React app in a real Chromium against the Vite dev server with the Tauri IPC
 * layer mocked (see tests/e2e/fixtures). Full-desktop smoke tests against the packaged binary are
 * driven by tests/desktop (tauri-driver) and are not part of this config.
 */
export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: 'http://localhost:1420',
    trace: 'on-first-retry',
    viewport: { width: 1280, height: 800 },
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'pnpm dev',
    url: 'http://localhost:1420',
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
});
