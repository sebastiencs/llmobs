import { STORAGE_PREFIX } from "@/product";

export type ColumnSpec = {
  id: string;
  locked?: boolean;
  defaultVisible?: boolean;
};

export function columnChoiceKey(tableId: string): string {
  return `${STORAGE_PREFIX}table.${tableId}.cols`;
}

/** Visible ids in display order. `cols` undefined uses each spec's default. */
export function visibleColumnIds(
  specs: readonly ColumnSpec[],
  cols: readonly string[] | undefined,
): string[] {
  const known = new Set(specs.map((spec) => spec.id));
  const picked = cols
    ? cols.filter((id) => known.has(id))
    : specs.filter((spec) => spec.defaultVisible).map((spec) => spec.id);
  const unique: string[] = [];
  for (const id of picked) {
    if (!unique.includes(id)) {
      unique.push(id);
    }
  }
  return insertLocked(specs, unique);
}

export function sameColumnIds(left: readonly string[], right: readonly string[]): boolean {
  return left.length === right.length && left.every((id, index) => id === right[index]);
}

/** Swaps `id` with its neighbor. Returns undefined when the move is out of range. */
export function reorderVisible(
  visible: readonly string[],
  id: string,
  delta: -1 | 1,
): string[] | undefined {
  const index = visible.indexOf(id);
  const target = index + delta;
  if (index < 0 || target < 0 || target >= visible.length) {
    return undefined;
  }
  const next = [...visible];
  const swap = next[target];
  if (swap === undefined) {
    return undefined;
  }
  next[target] = id;
  next[index] = swap;
  return next;
}

export function readColumnChoice(tableId: string): string[] | undefined {
  try {
    const raw = localStorage.getItem(columnChoiceKey(tableId));
    if (!raw) {
      return undefined;
    }
    const ids = raw
      .split(",")
      .map((part) => part.trim())
      .filter((part) => part.length > 0);
    return ids.length > 0 ? ids : undefined;
  } catch {
    return undefined;
  }
}

export function writeColumnChoice(tableId: string, ids: readonly string[] | undefined): void {
  try {
    const key = columnChoiceKey(tableId);
    if (!ids || ids.length === 0) {
      localStorage.removeItem(key);
      return;
    }
    localStorage.setItem(key, ids.join(","));
  } catch {
    // Private mode and blocked storage leave the URL choice in place.
  }
}

function insertLocked(specs: readonly ColumnSpec[], picked: string[]): string[] {
  const result = [...picked];
  for (const spec of specs) {
    if (!spec.locked || result.includes(spec.id)) {
      continue;
    }
    const specIndex = specs.findIndex((item) => item.id === spec.id);
    let insertAt = result.length;
    for (let index = 0; index < result.length; index += 1) {
      const other = specs.findIndex((item) => item.id === result[index]);
      if (other > specIndex) {
        insertAt = index;
        break;
      }
    }
    result.splice(insertAt, 0, spec.id);
  }
  return result;
}
