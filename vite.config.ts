import { fileURLToPath, URL } from 'node:url';

import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

// Tauri injects TAURI_DEV_HOST when developing against a remote device; unused on desktop.
const host = process.env['TAURI_DEV_HOST'];
const isDebug = Boolean(process.env['TAURI_ENV_DEBUG']);

export default defineConfig({
  plugins: [
    react({
      // React Compiler memoizes automatically, which is what lets the codebase avoid scattering
      // useMemo/useCallback without evidence (§88). Backed by oxc-transform-react in plugin v6.
      compiler: true,
    }),
    tailwindcss(),
  ],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  // Tauri expects a fixed port and fails if it is not available.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host ?? false,
    // Spread rather than assigning `undefined`: exactOptionalPropertyTypes distinguishes an absent
    // property from one explicitly set to undefined.
    ...(host ? { hmr: { protocol: 'ws' as const, host, port: 1421 } } : {}),
    watch: {
      ignored: ['**/src-tauri/**', '**/crates/**', '**/target/**', '**/docs/**'],
    },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    // WebView2 Evergreen is Chromium-based; Chrome 105 is Tauri's documented floor.
    target: 'chrome105',
    // Vite 8 transpiles and minifies with Oxc; the esbuild path is deprecated and no longer
    // bundled. `true` selects the built-in minifier.
    minify: !isDebug,
    sourcemap: isDebug,
    chunkSizeWarningLimit: 1500,
    rollupOptions: {
      output: {
        // Split the two large, rarely-changing dependencies into their own chunks so an
        // application-code change does not invalidate them in the webview's cache.
        manualChunks: (id: string) => {
          if (id.includes('node_modules/shaka-player')) return 'shaka';
          if (id.includes('node_modules/react-dom') || id.includes('node_modules/react/')) {
            return 'react';
          }
          return undefined;
        },
      },
    },
  },
});
