export type JsonKind = "object" | "array" | "string" | "number" | "boolean" | "null";

/** One node of a JSON value, in pre-order. `end` is the index past the last descendant. */
export type JsonRow = {
  path: string;
  depth: number;
  key: string;
  kind: JsonKind;
  preview: string;
  childCount: number;
  end: number;
};

const PATH_SEP = "\0";

type Task = {
  value: unknown;
  depth: number;
  key: string;
  path: string;
};

/**
 * Flatten `value` in pre-order. The walk is a heap stack, so a payload
 * thousands of levels deep does not overflow the call stack.
 * Long strings are stored whole; the viewer truncates them.
 */
export function jsonRows(value: unknown): JsonRow[] {
  const rows: JsonRow[] = [];
  const tasks: Task[] = [{ value, depth: 0, key: "", path: "" }];
  while (tasks.length > 0) {
    const task = tasks.pop();
    if (!task) {
      break;
    }
    const kind = jsonKind(task.value);
    const index = rows.length;
    if (kind === "object" || kind === "array") {
      const children = childEntries(task.value, kind);
      rows.push({
        path: task.path,
        depth: task.depth,
        key: task.key,
        kind,
        preview: containerPreview(kind, children.length),
        childCount: children.length,
        end: index + 1,
      });
      for (let childIndex = children.length - 1; childIndex >= 0; childIndex--) {
        const child = children[childIndex];
        if (!child) {
          continue;
        }
        tasks.push({
          value: child.value,
          depth: task.depth + 1,
          key: child.key,
          path: task.path.length === 0 ? child.key : `${task.path}${PATH_SEP}${child.key}`,
        });
      }
    } else {
      rows.push({
        path: task.path,
        depth: task.depth,
        key: task.key,
        kind,
        preview: primitivePreview(task.value, kind),
        childCount: 0,
        end: index + 1,
      });
    }
  }
  assignEnds(rows);
  return rows;
}

/** Paths of the ancestors of `path`, root first. The root path is `""`. */
export function ancestorPaths(path: string): string[] {
  if (path.length === 0) {
    return [];
  }
  const ancestors = [""];
  let start = 0;
  while (start < path.length) {
    const at = path.indexOf(PATH_SEP, start);
    if (at < 0) {
      break;
    }
    ancestors.push(path.slice(0, at));
    start = at + PATH_SEP.length;
  }
  return ancestors;
}

/** Indices to render, skipping descendants of collapsed paths. */
export function visibleJsonRows(
  rows: readonly JsonRow[],
  collapsed: ReadonlySet<string>,
): number[] {
  const visible: number[] = [];
  let index = 0;
  while (index < rows.length) {
    const row = rows[index];
    if (!row) {
      break;
    }
    visible.push(index);
    index = collapsed.has(row.path) ? row.end : index + 1;
  }
  return visible;
}

/** Containers at `depth` or deeper start collapsed. */
export function defaultCollapsedPaths(rows: readonly JsonRow[], depth: number): Set<string> {
  const collapsed = new Set<string>();
  for (const row of rows) {
    if (row.childCount > 0 && row.depth >= depth) {
      collapsed.add(row.path);
    }
  }
  return collapsed;
}

function jsonKind(value: unknown): JsonKind {
  if (value === null || value === undefined) {
    return "null";
  }
  if (Array.isArray(value)) {
    return "array";
  }
  if (typeof value === "string") {
    return "string";
  }
  if (typeof value === "number") {
    return "number";
  }
  if (typeof value === "boolean") {
    return "boolean";
  }
  return "object";
}

function childEntries(value: unknown, kind: "object" | "array"): { key: string; value: unknown }[] {
  if (kind === "array" && Array.isArray(value)) {
    const children: { key: string; value: unknown }[] = [];
    for (let index = 0; index < value.length; index++) {
      children.push({ key: String(index), value: index in value ? value[index] : null });
    }
    return children;
  }
  if (!value || typeof value !== "object") {
    return [];
  }
  return Object.entries(value).map(([key, child]) => ({ key, value: child }));
}

function containerPreview(kind: "object" | "array", count: number): string {
  if (kind === "array") {
    return count === 1 ? "[…] 1 item" : `[…] ${count} items`;
  }
  return count === 1 ? "{…} 1 key" : `{…} ${count} keys`;
}

function primitivePreview(value: unknown, kind: JsonKind): string {
  if (kind === "string" && typeof value === "string") {
    return value;
  }
  if (kind === "number" && typeof value === "number" && Number.isFinite(value)) {
    return JSON.stringify(value);
  }
  if (kind === "boolean") {
    return value === true ? "true" : "false";
  }
  return "null";
}

function assignEnds(rows: JsonRow[]) {
  const stack: number[] = [];
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index];
    if (!row) {
      continue;
    }
    while (stack.length > 0) {
      const parent = stack[stack.length - 1];
      const parentRow = parent === undefined ? undefined : rows[parent];
      if (parent === undefined || !parentRow || parentRow.depth < row.depth) {
        break;
      }
      stack.pop();
      parentRow.end = index;
    }
    stack.push(index);
  }
  while (stack.length > 0) {
    const parent = stack.pop();
    const parentRow = parent === undefined ? undefined : rows[parent];
    if (parentRow) {
      parentRow.end = rows.length;
    }
  }
}
