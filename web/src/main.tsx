import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
import {
  applyResolvedTheme,
  readStoredTheme,
  resolveTheme,
  ThemeProvider,
} from "@/components/theme-provider";
import { ThemeToggle } from "@/components/theme-toggle";
import { PRODUCT_NAME } from "@/product";
import "./styles.css";

applyResolvedTheme(resolveTheme(readStoredTheme()));

const root = document.getElementById("root");
if (!root) {
  throw new Error("Missing #root");
}

createRoot(root).render(
  <StrictMode>
    <ThemeProvider>
      <header className="flex h-14 items-center justify-between border-b px-4">
        <span className="font-medium">{PRODUCT_NAME}</span>
        <ThemeToggle />
      </header>
    </ThemeProvider>
  </StrictMode>,
);
