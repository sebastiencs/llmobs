import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { formatRelative, formatTimestamp } from "@/lib/format";
import { useTimeZone } from "@/lib/settings";

export function TimeText({ value, style }: { value: string; style: "time" | "datetime" }) {
  const zone = useTimeZone();
  const other = zone === "utc" ? "local" : "utc";
  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger asChild>
          <time dateTime={value} data-zone={zone} className="tabular-nums">
            {formatTimestamp(value, zone, style)}
          </time>
        </TooltipTrigger>
        <TooltipContent className="flex-col items-start">
          <span>{value}</span>
          <span>{formatTimestamp(value, other, "datetime")}</span>
          <span>{formatRelative(value, Date.now())}</span>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}
