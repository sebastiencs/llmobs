import { readFileSync } from "node:fs";
import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import tanstackRouter from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const workerPath = fileURLToPath(new URL("./msw/mockServiceWorker.js", import.meta.url));

function mockServiceWorker(): Plugin {
  return {
    name: "llmobs-mock-service-worker",
    apply: "serve",
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const url = request.url?.split("?")[0];
        if (url !== "/mockServiceWorker.js") {
          next();
          return;
        }
        response.setHeader("Content-Type", "text/javascript");
        response.end(readFileSync(workerPath));
      });
    },
  };
}

export default defineConfig(({ mode }) => ({
  plugins: [
    mockServiceWorker(),
    tanstackRouter({ target: "react", autoCodeSplitting: true }),
    react(),
    tailwindcss(),
  ],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    fs: {
      allow: [repoRoot],
    },
    ...(mode === "server"
      ? {
          proxy: {
            "/api": "http://localhost:8080",
            "/v1": "http://localhost:8080",
          },
        }
      : {}),
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    restoreMocks: true,
    testTimeout: 15_000,
    hookTimeout: 30_000,
  },
}));
