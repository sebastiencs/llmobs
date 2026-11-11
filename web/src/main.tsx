import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import { createAppQueryClient } from "@/api/client";
import {
  applyResolvedTheme,
  readStoredTheme,
  resolveTheme,
  ThemeProvider,
} from "@/components/theme-provider";
import { createAppRouter } from "@/router";
import "./styles.css";

async function main(): Promise<void> {
  applyResolvedTheme(resolveTheme(readStoredTheme()));

  if (import.meta.env.DEV && import.meta.env.MODE !== "server") {
    const { isCommonAssetRequest } = await import("msw");
    const { HttpNetworkFrame } = await import("msw/experimental");
    const { readMockMode, worker } = await import("./mocks/browser");
    if (readMockMode() === "large") {
      const [{ createDataset }, { largeHandlers }] = await Promise.all([
        import("./mocks/generated/dataset"),
        import("./mocks/generated/handlers"),
      ]);
      worker.use(...largeHandlers(createDataset({ seed: 1, now: Date.now(), traces: 5000 })));
    }
    await worker.start({
      quiet: false,
      // The document request for `/` is not an API call. Warn on anything else
      // that the fixtures do not cover. Common assets stay quiet, as with "warn".
      onUnhandledFrame({ frame, defaults }) {
        if (!(frame instanceof HttpNetworkFrame)) {
          defaults.warn();
          return;
        }
        const { request } = frame.data;
        if (request.mode === "navigate" || isCommonAssetRequest(request)) {
          return;
        }
        defaults.warn();
      },
    });
  }

  const root = document.getElementById("root");
  if (!root) {
    throw new Error("Missing #root");
  }

  const queryClient = createAppQueryClient();
  const router = createAppRouter({ queryClient });

  createRoot(root).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <ThemeProvider>
          <RouterProvider router={router} />
        </ThemeProvider>
      </QueryClientProvider>
    </StrictMode>,
  );
}

void main();
