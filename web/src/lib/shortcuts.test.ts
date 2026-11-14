import { expect, it } from "vitest";
import {
  goDestination,
  hexId,
  isTypingTarget,
  matchesBinding,
  reduceShortcut,
  SHORTCUTS,
} from "@/lib/shortcuts";

const plain = { metaKey: false, ctrlKey: false, altKey: false };

it("lists every shortcut the help dialog shows", () => {
  expect(SHORTCUTS.map((row) => row.keys)).toEqual([
    "/",
    "f",
    "j / k, ↓ / ↑",
    "Enter",
    "Esc",
    "t",
    "0",
    "⌘↵",
    "g then t / s / e / u / d",
    "⌘K",
    "?",
  ]);
});

it("recognizes keys, chords, and modifiers", () => {
  expect(reduceShortcut(null, { ...plain, key: "j" }, false)).toEqual({
    chord: null,
    action: "row-next",
  });
  expect(reduceShortcut(null, { ...plain, key: "ArrowUp" }, false).action).toBe("row-prev");
  expect(reduceShortcut(null, { ...plain, key: "?" }, false).action).toBe("help");
  expect(reduceShortcut(null, { ...plain, key: "g" }, false)).toEqual({
    chord: "g",
    action: null,
  });
  expect(reduceShortcut("g", { ...plain, key: "s" }, false).action).toBe("go-spans");
  expect(reduceShortcut("g", { ...plain, key: "e" }, false).action).toBe("go-sessions");
  expect(reduceShortcut("g", { ...plain, key: "d" }, false).action).toBe("go-dashboard");
  expect(reduceShortcut("g", { ...plain, key: "j" }, false)).toEqual({
    chord: null,
    action: "row-next",
  });
  expect(reduceShortcut(null, { ...plain, key: "k", metaKey: true }, true)).toEqual({
    chord: null,
    action: "palette",
  });
  expect(reduceShortcut(null, { ...plain, key: "k", ctrlKey: true }, false).action).toBe("palette");
  expect(reduceShortcut("g", { ...plain, key: "s" }, true)).toEqual({
    chord: null,
    action: null,
  });
  expect(goDestination("go-users")).toBe("/$org/$project/users");
  expect(matchesBinding({ ...plain, key: "0" }, "fit")).toBe(true);
  expect(matchesBinding({ ...plain, key: "k", metaKey: true }, "row-prev")).toBe(false);
});

it("accepts a trace or span id with dashes and any case", () => {
  expect(hexId("A1000000000000000000000000000001", 32)).toBe("a1000000000000000000000000000001");
  expect(hexId("a1000000-0000-0000-0000-000000000001", 32)).toBe(
    "a1000000000000000000000000000001",
  );
  expect(hexId("B100000000000013", 16)).toBe("b100000000000013");
  expect(hexId("b100000000000013", 32)).toBeUndefined();
  expect(hexId("not-an-id", 32)).toBeUndefined();
});

it("treats fields and the code editor as typing", () => {
  const input = document.createElement("input");
  const editor = document.createElement("div");
  editor.className = "cm-editor";
  const child = document.createElement("span");
  editor.append(child);
  document.body.append(input, editor);
  expect(isTypingTarget(input)).toBe(true);
  expect(isTypingTarget(child)).toBe(true);
  expect(isTypingTarget(document.body)).toBe(false);
  input.remove();
  editor.remove();
});
