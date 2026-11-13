import { Loader2 } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { Button } from "@/components/ui/button";

export function SubmitButton({
  pending,
  children,
  disabled,
  form,
  variant,
}: {
  pending: boolean;
  children: ReactNode;
  disabled?: boolean;
  form?: string;
  variant?: ComponentProps<typeof Button>["variant"];
}) {
  return (
    <Button type="submit" form={form} variant={variant} disabled={pending || disabled}>
      {pending ? <Loader2 className="animate-spin" aria-hidden="true" /> : null}
      {children}
    </Button>
  );
}
