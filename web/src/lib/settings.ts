import { useSyncExternalStore } from "react";
import { STORAGE_PREFIX } from "@/product";

export type TimeZoneSetting = "local" | "utc";

export const TIME_ZONE_STORAGE_KEY = `${STORAGE_PREFIX}time_zone`;

const listeners = new Set<() => void>();
let fallback: TimeZoneSetting | null = null;

function readStored(): TimeZoneSetting {
  try {
    const value = localStorage.getItem(TIME_ZONE_STORAGE_KEY);
    if (value === "utc" || value === "local") {
      return value;
    }
    return "local";
  } catch {
    return fallback ?? "local";
  }
}

function emit(): void {
  for (const listener of listeners) {
    listener();
  }
}

export function getTimeZone(): TimeZoneSetting {
  return readStored();
}

export function setTimeZone(next: TimeZoneSetting): void {
  fallback = next;
  try {
    localStorage.setItem(TIME_ZONE_STORAGE_KEY, next);
  } catch {
    // Private mode can throw. The choice still applies for this page.
  }
  emit();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useTimeZone(): TimeZoneSetting {
  return useSyncExternalStore(subscribe, getTimeZone, () => "local");
}
