/**
 * UUIDv7 (RFC 9562 §5.7).
 *
 * 48 big-endian bits of Unix milliseconds from `Date.now()`, version `0b0111`,
 * 12 random bits (`rand_a`), variant `0b10`, 62 random bits (`rand_b`); every
 * random bit comes from `crypto.getRandomValues` of the running runtime.
 */

import { runtimeGlobals } from "./web.js";

/**
 * A new UUIDv7 as a lowercase string. Throws when the runtime has no
 * `crypto.getRandomValues`; `score()` checks for that case first and warns
 * once instead (§ Score request).
 */
export function uuid7(): string {
  const { crypto } = runtimeGlobals();
  if (crypto === undefined) {
    throw new Error("crypto is not available");
  }
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  const ms = Date.now();
  bytes[0] = Math.floor(ms / 2 ** 40) & 0xff;
  bytes[1] = Math.floor(ms / 2 ** 32) & 0xff;
  bytes[2] = Math.floor(ms / 2 ** 24) & 0xff;
  bytes[3] = Math.floor(ms / 2 ** 16) & 0xff;
  bytes[4] = Math.floor(ms / 2 ** 8) & 0xff;
  bytes[5] = ms & 0xff;
  // Version 7 over the high nibble; the low nibble stays random (rand_a).
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x70;
  // Variant 10xx over the two high bits; the six low bits stay random (rand_b).
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
