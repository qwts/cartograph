/// <reference types="vitest/config" />
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Port 1420 is fixed by src-tauri/tauri.conf.json (devUrl); strictPort makes a
// clash fail loudly instead of silently drifting to a port Tauri won't load.
import path from 'node:path';
import { storybookTest } from '@storybook/addon-vitest/vitest-plugin';
import { playwright } from '@vitest/browser-playwright';

// More info at: https://storybook.js.org/docs/next/writing-tests/integrations/vitest-addon
export default defineConfig(({ mode }) => ({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true
  },
  // Production hardening (#221): no source maps in shipped assets, and strip
  // `console.*` + `debugger` from the bundle. Vite 8 minifies with oxc (the
  // `esbuild` option is ignored), so this goes through the rolldown minifier.
  // In a production Tauri build the webview devtools are disabled, so console
  // output is invisible anyway; dropping it avoids leaking internal strings.
  // Dev/test builds are untouched (minify defaults apply).
  build: {
    sourcemap: false,
    rolldownOptions:
      mode === 'production'
        ? { output: { minify: { compress: { dropConsole: true, dropDebugger: true }, mangle: true } } }
        : {}
  },
  test: {
    projects: [{
      // Plain unit tests (Node) — the storybook project below does not pick
      // these up, so they need their own project or `vitest run` skips them.
      extends: true,
      test: {
        name: 'unit',
        environment: 'node',
        include: ['src/**/*.test.ts'],
      },
    }, {
      extends: true,
      plugins: [
      // The plugin will run tests for the stories defined in your Storybook config
      // See options at: https://storybook.js.org/docs/next/writing-tests/integrations/vitest-addon#storybooktest
      storybookTest({
        configDir: path.join(import.meta.dirname, '.storybook')
      })],
      test: {
        name: 'storybook',
        browser: {
          enabled: true,
          headless: true,
          provider: playwright({}),
          instances: [{
            browser: 'chromium'
          }]
        }
      }
    }]
  }
}));