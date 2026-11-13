import { type ReactElement, useId } from "react";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/** Wraps a disabled control so keyboard and screen-reader users still learn why (D-L-4). */
export function DisabledReason({
  reason,
  children,
}: {
  reason: string | undefined;
  children: ReactElement;
}) {
  const id = useId();
  if (!reason) {
    return children;
  }
  return (
    <>
      <Tooltip>
        <TooltipTrigger asChild>
          {/* biome-ignore lint/a11y/noNoninteractiveTabindex: the disabled control inside cannot take focus, so the wrapper does. */}
          <span tabIndex={0} className="inline-flex w-fit" aria-describedby={id}>
            {children}
          </span>
        </TooltipTrigger>
        <TooltipContent>{reason}</TooltipContent>
      </Tooltip>
      <span id={id} hidden>
        {reason}
      </span>
    </>
  );
}
