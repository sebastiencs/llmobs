import { Check, Copy } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";

export function CopyButton({
  value,
  label,
  onCopied,
}: {
  value: string;
  label: string;
  onCopied?: () => void;
}) {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) {
      return;
    }
    const timer = window.setTimeout(() => {
      setCopied(false);
    }, 1_500);
    return () => {
      window.clearTimeout(timer);
    };
  }, [copied]);

  return (
    <Button
      type="button"
      variant="ghost"
      size="icon-xs"
      aria-label={`Copy ${label}`}
      onClick={() => {
        void navigator.clipboard.writeText(value).then(
          () => {
            setCopied(true);
            onCopied?.();
          },
          () => {
            // The clipboard can reject the write. Leave the icon as it is.
          },
        );
      }}
    >
      {copied ? <Check /> : <Copy />}
    </Button>
  );
}
