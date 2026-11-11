import { useEffect, useRef } from "react";
import { registerShortcut, type ShortcutId } from "@/lib/shortcuts";

export function useShortcut(id: ShortcutId, handler: (event: KeyboardEvent) => void): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => registerShortcut(id, (event) => ref.current(event)), [id]);
}
