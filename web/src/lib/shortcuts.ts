export const GO_TARGETS = {
  t: { id: "go-traces", to: "/$org/$project/traces" },
  s: { id: "go-spans", to: "/$org/$project/spans" },
  e: { id: "go-sessions", to: "/$org/$project/sessions" },
  u: { id: "go-users", to: "/$org/$project/users" },
  d: { id: "go-dashboard", to: "/$org/$project/dashboard" },
} as const;

type GoTarget = (typeof GO_TARGETS)[keyof typeof GO_TARGETS];

export type ShortcutId =
  | "focus-search"
  | "filter-text"
  | "row-next"
  | "row-prev"
  | "open"
  | "dismiss"
  | "toggle-mode"
  | "fit"
  | "palette"
  | "help"
  | GoTarget["id"];

type Binding = { id: ShortcutId; key: string; mod?: boolean };

export type Shortcut = {
  keys: string;
  description: string;
  scope: string;
  bindings: readonly Binding[];
};

/** One row per shortcut. The help dialog and the key listener both read this. */
export const SHORTCUTS: readonly Shortcut[] = [
  {
    keys: "/",
    description: "Focus search",
    scope: "lists",
    bindings: [{ id: "focus-search", key: "/" }],
  },
  {
    keys: "f",
    description: "Edit the filter as text",
    scope: "lists",
    bindings: [{ id: "filter-text", key: "f" }],
  },
  {
    keys: "j / k, ↓ / ↑",
    description: "Next / previous row",
    scope: "lists, tree",
    bindings: [
      { id: "row-next", key: "j" },
      { id: "row-next", key: "ArrowDown" },
      { id: "row-prev", key: "k" },
      { id: "row-prev", key: "ArrowUp" },
    ],
  },
  {
    keys: "Enter",
    description: "Open",
    scope: "lists",
    bindings: [{ id: "open", key: "Enter" }],
  },
  {
    keys: "Esc",
    description: "Close the panel, clear focus",
    scope: "anywhere",
    bindings: [{ id: "dismiss", key: "Escape" }],
  },
  {
    keys: "t",
    description: "Toggle Tree / Transcript",
    scope: "trace view",
    bindings: [{ id: "toggle-mode", key: "t" }],
  },
  {
    keys: "0",
    description: "Fit the timeline",
    scope: "trace view",
    bindings: [{ id: "fit", key: "0" }],
  },
  {
    keys: "g then t / s / e / u / d",
    description: "Go to traces, spans, sessions, users, or dashboard",
    scope: "project",
    bindings: [],
  },
  {
    keys: "⌘K",
    description: "Command palette",
    scope: "anywhere",
    bindings: [{ id: "palette", key: "k", mod: true }],
  },
  {
    keys: "?",
    description: "Shortcuts help",
    scope: "anywhere",
    bindings: [{ id: "help", key: "?" }],
  },
];

const ELEMENT_SHORTCUTS: ReadonlySet<ShortcutId> = new Set(["row-next", "row-prev", "open", "fit"]);

export type Chord = "g" | null;

export type KeyInfo = {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
};

export function isElementShortcut(id: ShortcutId): boolean {
  return ELEMENT_SHORTCUTS.has(id);
}

export function goDestination(id: ShortcutId): GoTarget["to"] | undefined {
  for (const target of Object.values(GO_TARGETS)) {
    if (target.id === id) {
      return target.to;
    }
  }
  return undefined;
}

/** 16 or 32 hex digits, any case, dashes ignored. */
export function hexId(text: string, length: 16 | 32): string | undefined {
  const stripped = text.trim().replaceAll("-", "");
  if (stripped.length !== length || !/^[0-9a-fA-F]+$/.test(stripped)) {
    return undefined;
  }
  return stripped.toLowerCase();
}

export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }
  if (target.isContentEditable) {
    return true;
  }
  return target.closest("input, textarea, select, [contenteditable], .cm-editor") !== null;
}

export function reduceShortcut(
  chord: Chord,
  key: KeyInfo,
  typing: boolean,
): { chord: Chord; action: ShortcutId | null } {
  const palette = key.key.toLowerCase() === "k" && (key.metaKey || key.ctrlKey) && !key.altKey;
  if (palette) {
    return { chord: null, action: "palette" };
  }
  if (typing || key.metaKey || key.ctrlKey || key.altKey) {
    return { chord: null, action: null };
  }
  if (chord === "g") {
    const go = GO_TARGETS[key.key as keyof typeof GO_TARGETS];
    if (go) {
      return { chord: null, action: go.id };
    }
  }
  if (key.key === "g") {
    return { chord: "g", action: null };
  }
  for (const row of SHORTCUTS) {
    for (const binding of row.bindings) {
      if (!binding.mod && binding.key === key.key) {
        return { chord: null, action: binding.id };
      }
    }
  }
  return { chord: null, action: null };
}

export function matchesBinding(event: KeyInfo, id: ShortcutId): boolean {
  return reduceShortcut(null, event, false).action === id;
}

type ShortcutHandler = (event: KeyboardEvent) => void;

const handlers = new Map<ShortcutId, Set<ShortcutHandler>>();

export function registerShortcut(id: ShortcutId, handler: ShortcutHandler): () => void {
  let set = handlers.get(id);
  if (!set) {
    set = new Set();
    handlers.set(id, set);
  }
  set.add(handler);
  return () => {
    set.delete(handler);
  };
}

export function dispatchShortcut(id: ShortcutId, event: KeyboardEvent): void {
  const set = handlers.get(id);
  if (!set) {
    return;
  }
  for (const handler of [...set]) {
    handler(event);
  }
}
