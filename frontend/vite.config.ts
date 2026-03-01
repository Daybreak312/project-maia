import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/ingest': 'http://localhost:8080',
      '/search': 'http://localhost:8080',
      '/documents': 'http://localhost:8080',
      '/recent': 'http://localhost:8080',
      '/tags': 'http://localhost:8080',
      '/health': 'http://localhost:8080',
      '/api': 'http://localhost:8080',
    },
  },
})
