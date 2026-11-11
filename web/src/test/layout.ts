/**
 * jsdom does not lay elements out. TanStack Virtual reads `clientWidth` and
 * `clientHeight` for the viewport, and `scrollHeight` / `scrollWidth` for the
 * maximum scroll offset. `offset*` and `getBoundingClientRect` are stubbed
 * too, for anything that measures the scroll container directly.
 *
 * Elements with `data-scroll-container` get the list viewport size. The chart
 * wrapper (`data-chart-container`) and the Recharts responsive container
 * inside it get a chart size. Descendants keep jsdom's own measurement:
 * sizing them too makes Recharts treat the legend as the whole chart and
 * give the plot no height.
 */

const WIDTH = 1200;
const HEIGHT = 800;
const SCROLL = 100_000;
const CHART_WIDTH = 640;
const CHART_HEIGHT = 180;

const SIZE = {
  clientHeight: HEIGHT,
  offsetHeight: HEIGHT,
  scrollHeight: SCROLL,
  clientWidth: WIDTH,
  offsetWidth: WIDTH,
  scrollWidth: SCROLL,
} as const;

type SizeName = keyof typeof SIZE;

const saved = new Map<SizeName, PropertyDescriptor | undefined>();
let savedRect: typeof HTMLElement.prototype.getBoundingClientRect | undefined;

function inChart(element: Element): boolean {
  return (
    element.hasAttribute("data-chart-container") ||
    element.classList.contains("recharts-responsive-container")
  );
}

function chartMeasurement(element: Element, name: SizeName): number | undefined {
  if (!inChart(element)) {
    return undefined;
  }
  if (name === "clientWidth" || name === "offsetWidth" || name === "scrollWidth") {
    return CHART_WIDTH;
  }
  if (name === "clientHeight" || name === "offsetHeight" || name === "scrollHeight") {
    return CHART_HEIGHT;
  }
  return undefined;
}

function fallbackGet(name: SizeName, element: Element): number {
  let proto = Object.getPrototypeOf(HTMLElement.prototype) as object | null;
  while (proto) {
    const descriptor = Object.getOwnPropertyDescriptor(proto, name);
    if (descriptor?.get) {
      return descriptor.get.call(element) as number;
    }
    proto = Object.getPrototypeOf(proto) as object | null;
  }
  return 0;
}

export function installLayout(): void {
  for (const name of Object.keys(SIZE) as SizeName[]) {
    saved.set(name, Object.getOwnPropertyDescriptor(HTMLElement.prototype, name));
    Object.defineProperty(HTMLElement.prototype, name, {
      configurable: true,
      get() {
        if (this instanceof Element && this.hasAttribute("data-scroll-container")) {
          return SIZE[name];
        }
        if (this instanceof Element) {
          const chart = chartMeasurement(this, name);
          if (chart !== undefined) {
            return chart;
          }
        }
        return fallbackGet(name, this);
      },
    });
  }
  savedRect = HTMLElement.prototype.getBoundingClientRect;
  HTMLElement.prototype.getBoundingClientRect = function getBoundingClientRect() {
    if (this.hasAttribute("data-scroll-container")) {
      return new DOMRect(0, 0, WIDTH, HEIGHT);
    }
    if (inChart(this)) {
      return new DOMRect(0, 0, CHART_WIDTH, CHART_HEIGHT);
    }
    return savedRect?.call(this) ?? new DOMRect();
  };
}

export function restoreLayout(): void {
  for (const [name, descriptor] of saved) {
    if (descriptor) {
      Object.defineProperty(HTMLElement.prototype, name, descriptor);
    } else {
      delete HTMLElement.prototype[name];
    }
  }
  saved.clear();
  if (savedRect) {
    HTMLElement.prototype.getBoundingClientRect = savedRect;
    savedRect = undefined;
  }
}
