type Navigate = (url: string) => void;

function browserNavigate(url: string): void {
  window.location.assign(url);
}

let current: Navigate = browserNavigate;

/** Leaves the app for `url` (a full URL outside the router). */
export function navigateExternal(url: string): void {
  current(url);
}

/** Tests replace the navigation; `undefined` restores the browser's. */
export function setExternalNavigator(next: Navigate | undefined): void {
  current = next ?? browserNavigate;
}
