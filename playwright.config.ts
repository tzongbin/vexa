import { defineConfig, devices } from '@playwright/test'

export default defineConfig({
  testDir: './tests/ui',
  fullyParallel: false,
  workers: 1,
  reporter: 'list',
  outputDir: 'output/playwright/test-results',
  timeout: 15_000,
  expect: { timeout: 5_000 },
  use: {
    baseURL: 'http://127.0.0.1:1421',
    trace: 'retain-on-failure',
  },
  projects: [
    {
      name: 'msedge',
      use: { ...devices['Desktop Chrome'], channel: 'msedge' },
    },
  ],
  webServer: {
    command: 'npm run dev -- --host 127.0.0.1 --port 1421 --strictPort',
    url: 'http://127.0.0.1:1421',
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
})
