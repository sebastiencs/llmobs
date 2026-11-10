import type { components } from "./schema.gen";

/** A component schema of the API by name, e.g. `Schema<"Project">`. */
export type Schema<Name extends keyof components["schemas"]> = components["schemas"][Name];
