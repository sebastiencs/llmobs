import { STORAGE_PREFIX } from "@/product";

export const SCHEMA_PANEL_KEY = `${STORAGE_PREFIX}sql.schema`;

/** Open unless the user closed it. */
export function readSchemaPanelOpen(): boolean {
  try {
    return localStorage.getItem(SCHEMA_PANEL_KEY) !== "closed";
  } catch {
    return true;
  }
}

export function writeSchemaPanelOpen(open: boolean): void {
  try {
    localStorage.setItem(SCHEMA_PANEL_KEY, open ? "open" : "closed");
  } catch {
    // Private mode: the choice still applies for this page.
  }
}
