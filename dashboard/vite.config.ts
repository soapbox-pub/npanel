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
      // The server's parsing rules, in TypeScript: twins of the Rust modules
      // they name, so the preview shown while typing is what the server will
      // store, and the www switch is offered exactly where it is honoured.
      "@shared/nsite-address": path.resolve(import.meta.dirname, "./src/shared/nsite-address.ts"),
      "@shared/pubkey": path.resolve(import.meta.dirname, "./src/shared/pubkey.ts"),
      "@shared/hostname": path.resolve(import.meta.dirname, "./src/shared/hostname.ts"),
      "@shared/nip05": path.resolve(import.meta.dirname, "./src/shared/nip05.ts"),
      "@shared/relay-url": path.resolve(import.meta.dirname, "./src/shared/relay-url.ts"),
      "@shared/analytics-query": path.resolve(import.meta.dirname, "./src/shared/analytics-query/parse.ts"),
    },
  },
}));
