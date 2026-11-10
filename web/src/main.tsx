import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
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
    const { worker } = await import("./mocks/browser");
    await worker.start({ onUnhandledFrame: "warn", quiet: false });
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
