import { STORAGE_PREFIX } from "@/product";

export const MIN_COLUMN_SIZE = 48;

export function columnSizeKey(tableId: string): string {
  return `${STORAGE_PREFIX}table.${tableId}`;
}

export function columnSizesAfterResize(
  sizes: Record<string, number>,
  id: string,
  next: number,
): Record<string, number> {
  return { ...sizes, [id]: Math.max(MIN_COLUMN_SIZE, Math.round(next)) };
}

export function columnSizesAfterReset(
  sizes: Record<string, number>,
  id: string,
): Record<string, number> {
  if (!(id in sizes)) {
    return sizes;
  }
  const next = { ...sizes };
  delete next[id];
  return next;
}

export function readColumnSizes(tableId: string): Record<string, number> {
  try {
    const raw = localStorage.getItem(columnSizeKey(tableId));
    if (!raw) {
      return {};
    }
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      return {};
    }
    const sizes: Record<string, number> = {};
    for (const [id, value] of Object.entries(parsed)) {
      if (typeof value === "number" && Number.isFinite(value)) {
        sizes[id] = value;
      }
    }
    return sizes;
  } catch {
    return {};
  }
}

export function writeColumnSizes(tableId: string, sizes: Record<string, number>): void {
  try {
    const key = columnSizeKey(tableId);
    if (Object.keys(sizes).length === 0) {
      localStorage.removeItem(key);
      return;
    }
    localStorage.setItem(key, JSON.stringify(sizes));
  } catch {
    // Private mode and blocked storage leave the in-memory sizes in place.
  }
}
