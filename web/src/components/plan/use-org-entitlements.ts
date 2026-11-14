import { useQuery } from "@tanstack/react-query";
import { orgEntitlementsQuery } from "@/api/queries-settings";
import type { Feature } from "./feature-label";

/** The org's entitlements; `allows(feature)` is false while loading or on error. */
export function useOrgEntitlements(org: string) {
  const query = useQuery(orgEntitlementsQuery(org));
  return {
    query,
    allows: (feature: Feature) => query.data?.features.includes(feature) ?? false,
    upgradeUrl: query.data?.upgrade_url,
  };
}
