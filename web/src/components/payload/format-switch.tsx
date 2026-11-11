import { useEffect, useState } from "react";
import { STORAGE_PREFIX } from "@/product";

export const PAYLOAD_FORMAT_KEY = `${STORAGE_PREFIX}payload-format`;

export type PayloadMode = "pretty" | "json" | "raw";

const MODES: { mode: PayloadMode; label: string }[] = [
  { mode: "pretty", label: "Pretty" },
  { mode: "json", label: "JSON" },
  { mode: "raw", label: "Raw" },
];

const listeners = new Set<() => void>();

export function readPayloadMode(): PayloadMode {
  const stored = localStorage.getItem(PAYLOAD_FORMAT_KEY);
  if (stored === "pretty" || stored === "json" || stored === "raw") {
    return stored;
  }
  return "pretty";
}

/** The Pretty / JSON / Raw choice, shared by every payload on the page and remembered per user. */
export function usePayloadFormat(): [PayloadMode, (mode: PayloadMode) => void] {
  const [mode, setMode] = useState(readPayloadMode);
  useEffect(() => {
    const listener = () => {
      setMode(readPayloadMode());
    };
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  }, []);
  const update = (next: PayloadMode) => {
    localStorage.setItem(PAYLOAD_FORMAT_KEY, next);
    setMode(next);
    for (const listener of listeners) {
      listener();
    }
  };
  return [mode, update];
}

export function FormatSwitch({
  value,
  onChange,
}: {
  value: PayloadMode;
  onChange: (mode: PayloadMode) => void;
}) {
  return (
    <fieldset className="inline-flex items-center gap-1 border-0 p-0">
      <legend className="sr-only">Payload format</legend>
      {MODES.map((item) => (
        <button
          key={item.mode}
          type="button"
          aria-pressed={value === item.mode}
          className="h-5 rounded px-1.5 text-[10px] text-muted-foreground aria-pressed:bg-muted aria-pressed:text-foreground"
          onClick={() => onChange(item.mode)}
        >
          {item.label}
        </button>
      ))}
    </fieldset>
  );
}
