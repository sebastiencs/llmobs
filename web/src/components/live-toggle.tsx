import { cn } from "cn";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { StreamStatus } from "@/lib/sse";

const LIVE_ROW_CSS = `
@keyframes live-row {
  from { box-shadow: inset 0 0 0 999px color-mix(in oklch, var(--primary) 28%, transparent); }
  to { box-shadow: inset 0 0 0 999px transparent; }
}
.live-row { animation: live-row 1.5s ease-out; }
@media (prefers-reduced-motion: reduce) {
  .live-row { animation: none; }
}
`;

const DOT: Record<Exclude<StreamStatus, "closed">, string> = {
  connecting: "bg-muted-foreground",
  open: "bg-status-ok",
  retrying: "bg-status-error",
};

export function LiveToggle({
  pressed,
  disabled,
  reason,
  status,
  onPressedChange,
}: {
  pressed: boolean;
  disabled: boolean;
  reason?: string;
  status: StreamStatus;
  onPressedChange: (on: boolean) => void;
}) {
  const control = (
    <span className="inline-flex shrink-0 items-center gap-1.5">
      {pressed && status !== "closed" ? <LiveDot status={status} /> : null}
      <Switch
        id="traces-live"
        size="sm"
        checked={pressed}
        disabled={disabled}
        onCheckedChange={onPressedChange}
      />
      <Label htmlFor="traces-live" className="text-xs font-normal">
        Live
      </Label>
    </span>
  );
  return (
    <>
      <style>{LIVE_ROW_CSS}</style>
      {reason ? (
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="inline-flex shrink-0">{control}</span>
          </TooltipTrigger>
          <TooltipContent>{reason}</TooltipContent>
        </Tooltip>
      ) : (
        control
      )}
    </>
  );
}

function LiveDot({ status }: { status: Exclude<StreamStatus, "closed"> }) {
  return (
    <span role="status" className="inline-flex">
      <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", DOT[status])} />
      <span className="sr-only">{status}</span>
    </span>
  );
}
