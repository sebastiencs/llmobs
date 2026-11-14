import type { Schema } from "@/api/types";

export type Feature = Schema<"Feature">;

/** What unlocks a feature, as shown next to a lock (D-P-15). */
export function featureOffer(feature: Feature): string {
  switch (feature) {
    case "project_roles":
      return "Available with the Teams add-on or an enterprise licence";
    case "sso_enforcement":
      return "Available with an enterprise licence";
  }
}
