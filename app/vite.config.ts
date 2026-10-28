import path from 'path';
import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import { readFileSync } from 'fs';
import { execSync } from 'child_process';

const appVersion: string = JSON.parse(readFileSync(path.resolve(import.meta.dirname, '../desktop/src-tauri/tauri.conf.json'), 'utf8')).version;

/// The commit the window was built from: what the sidebar shows instead of a
/// release number, which means nothing for a fork that ships by hash.
const gitCommit = (() => {
  try {
    return execSync('git rev-parse --short HEAD', { cwd: path.resolve(import.meta.dirname, '..'), stdio: ['ignore', 'pipe', 'ignore'] })
      .toString()
      .trim();
  } catch {
    return '';
  }
})();

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, '.', '');
  return {
    server: {
      port: 3791,
      host: '0.0.0.0',
      proxy: {
        '/v1': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/setup': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/engine': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/health': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        '/mcp': {
          target: 'http://127.0.0.1:8791',
          changeOrigin: true,
        },
        // There is deliberately no proxy for the retired ACE Node service:
        // the studio talks to the native Rust server only, so a stray legacy
        // request fails loudly in development instead of silently 500-ing.
      },
    },
    build: {
      // Tailwind's palette is oklch(); WebView2 before 111 (the last one for
      // Windows 7 and 8.1 is 109) drops those colours and every surface turns
      // transparent. Lightning CSS writes a plain colour first for it.
      cssTarget: 'chrome100',
      cssMinify: 'lightningcss',
      rollupOptions: {
        // the visualiser's own window is a second page
        input: {
          main: path.resolve(import.meta.dirname, 'index.html'),
          visualizer: path.resolve(import.meta.dirname, 'visualizer.html'),
        },
      },
    },
    optimizeDeps: {
      exclude: ['@ffmpeg/ffmpeg', '@ffmpeg/util'],
    },
    define: {
      __APP_VERSION__: JSON.stringify(appVersion),
      __GIT_COMMIT__: JSON.stringify(gitCommit),
    },
    plugins: [react(), tailwindcss()],
    resolve: {
      alias: {
        '@': path.resolve(import.meta.dirname, '.'),
      }
    }
  };
});
