import { Lock } from "lucide-react";
import { type Feature, featureOffer } from "./feature-label";
import { UpgradeButton } from "./upgrade-button";

/** A lock line for a feature the org cannot use: the offer and an Upgrade button (D-P-15). */
export function LockedFeature({
  feature,
  upgradeUrl,
}: {
  feature: Feature;
  upgradeUrl: string | undefined;
}) {
  return (
    <div
      role="note"
      aria-label="Locked feature"
      className="flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm"
    >
      <Lock className="size-4 text-muted-foreground" aria-hidden="true" />
      <span>{featureOffer(feature)}.</span>
      <UpgradeButton url={upgradeUrl} />
    </div>
  );
}
