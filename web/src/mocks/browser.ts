import { setupWorker } from "msw/browser";
import { STORAGE_PREFIX } from "@/product";
import { handlers } from "./handlers";
import { savedViewHandlers, seedSavedViews } from "./saved-views";

export const MOCK_STORAGE_KEY = `${STORAGE_PREFIX}mock`;

export const worker = setupWorker(...savedViewHandlers(seedSavedViews()), ...handlers);

/** `mock=large` remembers the seeded dataset; `mock=fixtures` clears it. */
export function readMockMode(): "large" | "fixtures" {
  try {
    const requested = new URLSearchParams(window.location.search).get("mock");
    if (requested === "fixtures") {
      sessionStorage.removeItem(MOCK_STORAGE_KEY);
      return "fixtures";
    }
    if (requested === "large") {
      sessionStorage.setItem(MOCK_STORAGE_KEY, "large");
      return "large";
    }
    return sessionStorage.getItem(MOCK_STORAGE_KEY) === "large" ? "large" : "fixtures";
  } catch {
    return "fixtures";
  }
}
