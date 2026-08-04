import { resolve } from "node:path";
import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [tanstackRouter({ target: "react", autoCodeSplitting: true }), react(), tailwindcss()],
  resolve: {
    alias: { "@": resolve(import.meta.dirname, "src") },
  },
  server: {
    port: 5173,
    // Dev only. In production Caddy serves the SPA and proxies /api itself,
    // so the browser always sees a single origin.
    proxy: {
      "/api": {
        target: process.env.COORDINATOR_URL ?? "http://localhost:3000",
        changeOrigin: false,
        ws: true,
      },
    },
  },
  build: {
    outDir: "dist",
    sourcemap: true,
  },
});
