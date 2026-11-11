/** How often the traces list refetches while the onboarding panel is showing. */
export const ONBOARDING_POLL_MS = 5_000;

let pollMs = ONBOARDING_POLL_MS;

export function onboardingPollMs(): number {
  return pollMs;
}

/** Tests pass a short interval. The route reads it on each render. */
export function setOnboardingPollMs(ms: number): void {
  pollMs = ms;
}
