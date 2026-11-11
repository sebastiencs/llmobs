import { CopyButton } from "@/components/copy-button";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { shortId } from "@/lib/format";

export function IdText({ value, kind }: { value: string; kind: string }) {
  return (
    <TooltipProvider>
      <span className="group/id inline-flex items-center gap-0.5">
        <Tooltip>
          <TooltipTrigger asChild>
            <span className="font-mono">{shortId(value)}</span>
          </TooltipTrigger>
          <TooltipContent>{value}</TooltipContent>
        </Tooltip>
        <span className="opacity-0 group-focus-within/id:opacity-100 group-hover/id:opacity-100">
          <CopyButton value={value} label={`${kind} ID`} />
        </span>
      </span>
    </TooltipProvider>
  );
}
