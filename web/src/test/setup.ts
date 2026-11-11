import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterAll, afterEach, beforeAll, beforeEach } from "vitest";
import { server } from "../mocks/node";
import { savedViewHandlers, seedSavedViews } from "../mocks/saved-views";
import { installTestEventSource } from "./event-source";
import { installLayout, restoreLayout } from "./layout";
import { installMatchMedia, resetMatchMedia } from "./match-media";

installTestEventSource();

beforeAll(() => {
  server.listen({ onUnhandledFrame: "error" });
});

installMatchMedia();
installLayout();

window.scrollTo = () => {};

if (!Element.prototype.hasPointerCapture) {
  Element.prototype.hasPointerCapture = () => false;
}
if (!Element.prototype.releasePointerCapture) {
  Element.prototype.releasePointerCapture = () => {};
}
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => {};
}
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class ResizeObserver {
    observe(): void {}
    unobserve(): void {}
    disconnect(): void {}
  };
}

beforeEach(() => {
  server.use(...savedViewHandlers(seedSavedViews()));
});

afterEach(() => {
  server.resetHandlers();
  cleanup();
  localStorage.clear();
  document.documentElement.className = "";
  document.documentElement.style.colorScheme = "";
  resetMatchMedia();
});

afterAll(() => {
  restoreLayout();
  server.close();
});
