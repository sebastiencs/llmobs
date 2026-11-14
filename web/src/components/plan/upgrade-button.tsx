import { ExternalLink } from "lucide-react";
import { Button } from "@/components/ui/button";

/** Links to the instance's upgrade page; renders nothing when the instance sets no URL. */
export function UpgradeButton({ url }: { url: string | undefined }) {
  if (url === undefined) {
    return null;
  }
  return (
    <Button asChild size="sm" variant="outline">
      <a href={url} target="_blank" rel="noopener noreferrer">
        Upgrade
        <ExternalLink aria-hidden="true" />
      </a>
    </Button>
  );
}
