import { useQuery } from "@tanstack/react-query";
import { orgEntitlementsQuery } from "@/api/queries-settings";
import type { Feature } from "./feature-label";

/**
 * The org's entitlements. `allows(feature)` is false while loading or on error.
 * `locked(feature)` is true only once the entitlements have loaded and list no such
 * feature, so a lock never flashes while loading; the server stays the authority.
 */
export function useOrgEntitlements(org: string) {
  const query = useQuery(orgEntitlementsQuery(org));
  return {
    query,
    allows: (feature: Feature) => query.data?.features.includes(feature) ?? false,
    locked: (feature: Feature) =>
      query.data !== undefined && !query.data.features.includes(feature),
    upgradeUrl: query.data?.upgrade_url,
  };
}
