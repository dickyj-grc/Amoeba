import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    // Ports match scripts/dev.sh: console on 3064, API on 8064.
    port: 3064,
    proxy: {
      '/admin': {
        target: 'http://127.0.0.1:8064',
        changeOrigin: true,
      },
      '/auth': {
        target: 'http://127.0.0.1:8064',
        changeOrigin: true,
      },
      '/v1': {
        target: 'http://127.0.0.1:8064',
        changeOrigin: true,
      },
    },
  },
});
