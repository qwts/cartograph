import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// Port 1420 is fixed by src-tauri/tauri.conf.json (devUrl); strictPort makes a
// clash fail loudly instead of silently drifting to a port Tauri won't load.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
});
