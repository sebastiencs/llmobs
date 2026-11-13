export type CliAuthSearch = { port?: string; state?: string; host?: string };

export type CliRequest = { port: number; state: string; host?: string };

const PORT = /^[1-9]\d{3,4}$/;
const STATE = /^[A-Za-z0-9_-]{1,256}$/;
/** D-L-9: at least one non-digit, so the router's JSON parsing (README § Conventions, Search params) never turns it into a number. */
const HAS_NON_DIGIT = /[^0-9]/;
const HOST = /^[A-Za-z0-9.-]{1,64}$/;

/**
 * The three values exactly as they appear in the raw query string.
 * TanStack Router JSON-parses search values (README § Conventions, Search params):
 * values like `port=54321` would come back as numbers and `host=1e3` as `1000`, and the router re-serialises
 * `location.searchStr` too. The route therefore reads `useRouter().history.location.search`, the raw string.
 */
export function cliAuthSearchFrom(searchStr: string): CliAuthSearch {
  const params = new URLSearchParams(searchStr);
  const out: CliAuthSearch = {};
  for (const key of ["port", "state", "host"] as const) {
    const value = params.get(key);
    if (value !== null) {
      out[key] = value;
    }
  }
  return out;
}

/** D-L-9. Any invalid value makes the whole request invalid. */
export function parseCliRequest(search: CliAuthSearch): CliRequest | undefined {
  const { port, state, host } = search;
  if (port === undefined || !PORT.test(port)) {
    return undefined;
  }
  const portNumber = Number(port);
  if (portNumber < 1024 || portNumber > 65535) {
    return undefined;
  }
  if (state === undefined || !STATE.test(state) || !HAS_NON_DIGIT.test(state)) {
    return undefined;
  }
  if (host !== undefined && !HOST.test(host)) {
    return undefined;
  }
  return host === undefined ? { port: portNumber, state } : { port: portNumber, state, host };
}

export function cliTokenName(request: CliRequest): string {
  return request.host ? `CLI on ${request.host}` : "CLI";
}

/** The CLI's callback. The host is always 127.0.0.1; only the port comes from the link (spec 08). */
export function cliCallbackUrl(
  request: CliRequest,
  result: { token: string } | { error: "access_denied" },
): string {
  const url = new URL(`http://127.0.0.1:${request.port}/callback`);
  if ("token" in result) {
    url.searchParams.set("token", result.token);
  } else {
    url.searchParams.set("error", result.error);
  }
  url.searchParams.set("state", request.state);
  return url.toString();
}
