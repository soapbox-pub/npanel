import path from "node:path";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// https://vitejs.dev/config/
export default defineConfig(() => ({
  server: {
    host: "::",
    port: 8080,
    // The dashboard talks to the npanel server; run `bun run dev` alongside it.
    proxy: {
      "/api": {
        target: "http://127.0.0.1:3000",
        changeOrigin: false,
      },
    },
    fs: {
      // Allow importing the shared address parser from the server's src/.
      allow: [path.resolve(import.meta.dirname, ".."), path.resolve(import.meta.dirname)],
    },
  },
  plugins: [
    react(),
    tailwindcss(),
  ],
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: './src/test/setup.ts',
    exclude: [
      '**/node_modules/**',
      '**/dist/**',
      '**/{vite,eslint}.config.*',
      '.agents/**',
    ],
    onConsoleLog(log) {
      return !log.includes("React Router Future Flag Warning");
    },
    server: {
      // Vitest runs under Bun (see bunfig.toml), and Bun's loader mis-resolves
      // zod's `export *` chain, leaving `z` undefined. Inlining it routes zod
      // through Vite's transform instead, which resolves it correctly.
      deps: { inline: ["zod"] },
    },
    env: {
      DEBUG_PRINT_LIMIT: '0', // Suppress DOM output that exceeds AI context windows
    },
  },
  resolve: {
    alias: {
      "@": path.resolve(import.meta.dirname, "./src"),
      // One address parser for the server and the dashboard, so the preview
      // shown while typing is exactly what the server will store.
      "@shared/nsite-address": path.resolve(import.meta.dirname, "../src/nsite-address.ts"),
      "@shared/pubkey": path.resolve(import.meta.dirname, "../src/pubkey.ts"),
      // The same Public Suffix List rules the server applies, so the dashboard
      // offers the www redirect exactly where the server will honour it.
      "@shared/hostname": path.resolve(import.meta.dirname, "../src/hostname.ts"),
    },
  },
}));
