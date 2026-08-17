import { execFileSync } from "node:child_process";
import { resolve } from "node:path";
import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import pkg from "./package.json" with { type: "json" };

// The image build passes GIT_SHA in, because the Docker context carries no
// .git; asking git directly is the local-development path. A build that can
// answer neither still ships — the UI drops the sha rather than the version.
const gitSha =
  process.env.GIT_SHA ||
  (() => {
    try {
      return execFileSync("git", ["rev-parse", "HEAD"], { encoding: "utf8" }).trim();
    } catch {
      return null;
    }
  })();

export default defineConfig({
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
    __GIT_SHA__: JSON.stringify(gitSha),
  },
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
