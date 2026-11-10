type ChangeListener = (event: MediaQueryListEvent) => void;

const COLOR_SCHEME_QUERY = "(prefers-color-scheme: dark)";

let prefersDark = false;
const listeners = new Set<ChangeListener>();

export function setPrefersDark(value: boolean, notify = true): void {
  prefersDark = value;
  if (!notify) {
    return;
  }
  const event = { matches: value, media: COLOR_SCHEME_QUERY } as MediaQueryListEvent;
  for (const listener of [...listeners]) {
    listener(event);
  }
}

export function resetMatchMedia(): void {
  prefersDark = false;
  listeners.clear();
}

export function installMatchMedia(): void {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string): MediaQueryList => ({
      get matches() {
        return query === COLOR_SCHEME_QUERY ? prefersDark : false;
      },
      media: query,
      onchange: null,
      addEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
        if (type === "change" && typeof listener === "function") {
          listeners.add(listener as ChangeListener);
        }
      },
      removeEventListener: (type: string, listener: EventListenerOrEventListenerObject) => {
        if (type === "change" && typeof listener === "function") {
          listeners.delete(listener as ChangeListener);
        }
      },
      addListener: (listener: ChangeListener) => {
        listeners.add(listener);
      },
      removeListener: (listener: ChangeListener) => {
        listeners.delete(listener);
      },
      dispatchEvent: () => false,
    }),
  });
}
