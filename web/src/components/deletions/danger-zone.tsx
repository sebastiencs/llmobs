import type { ReactNode } from "react";
import { useId } from "react";
import { DisabledReason } from "@/components/disabled-reason";
import { Button } from "@/components/ui/button";

/** A destructive action set apart at the foot of a settings page. */
export function DangerZone({
  title,
  description,
  buttonLabel,
  disabledReason,
  disabled,
  onOpen,
  children,
}: {
  /** For example `Delete project`. */
  title: string;
  description: ReactNode;
  /** For example `Delete project…`. */
  buttonLabel: string;
  /** Set → the button is disabled with this reason (D-L-4). */
  disabledReason?: string;
  /** Disabled without a reason, for example while the role is still loading. */
  disabled?: boolean;
  onOpen: () => void;
  /** The confirmation dialog. */
  children?: ReactNode;
}) {
  const headingId = useId();
  return (
    <section
      aria-labelledby={headingId}
      className="flex max-w-md flex-col gap-3 rounded-md border border-destructive/50 p-4"
    >
      <h2 id={headingId} className="text-base font-medium">
        Danger zone
      </h2>
      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-1">
          <p className="font-medium">{title}</p>
          <p className="text-sm text-muted-foreground">{description}</p>
        </div>
        <div>
          <DisabledReason reason={disabledReason}>
            <Button
              type="button"
              variant="destructive"
              disabled={disabled === true || disabledReason !== undefined}
              onClick={onOpen}
            >
              {buttonLabel}
            </Button>
          </DisabledReason>
        </div>
      </div>
      {children}
    </section>
  );
}
