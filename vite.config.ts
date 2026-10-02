import { defineConfig } from 'vite'
import tailwindcss from '@tailwindcss/vite'

// Tauri espera porta fixa; o front-end mora em ui/ e o build vai para dist/.
export default defineConfig({
  root: 'ui',
  plugins: [tailwindcss()],
  clearScreen: false,
  server: { port: 5173, strictPort: true, host: '127.0.0.1' },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: { outDir: '../dist', emptyOutDir: true, target: 'safari16', sourcemap: false },
})
