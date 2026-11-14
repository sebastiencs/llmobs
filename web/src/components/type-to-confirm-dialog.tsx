import type { ReactNode } from "react";
import { useId, useRef, useState } from "react";
import { FormError } from "@/components/form/form-error";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

type TypeToConfirmProps = {
  title: string;
  description: ReactNode;
  /** The exact text the user must type (a slug or a user id). */
  expected: string;
  /** For example `Type the project slug "support-bot" to confirm`. */
  inputLabel: string;
  confirmLabel: string;
  pending: boolean;
  error?: string;
  onConfirm: () => void;
};

/** A destructive confirmation that needs the exact text (a slug or a user id) typed first. */
export function TypeToConfirmDialog({
  open,
  onOpenChange,
  ...props
}: TypeToConfirmProps & {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const contentRef = useRef<HTMLDivElement>(null);
  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        // Esc and the outside click must not close the dialog while the request runs.
        if (!props.pending) {
          onOpenChange(next);
        }
      }}
    >
      <AlertDialogContent
        ref={contentRef}
        onOpenAutoFocus={(event) => {
          // Radix focuses Cancel by default. Focus the input so the user can type straight away.
          event.preventDefault();
          contentRef.current?.querySelector("input")?.focus();
        }}
      >
        <TypeToConfirmBody {...props} />
      </AlertDialogContent>
    </AlertDialog>
  );
}

// Radix unmounts closed content, so the input and its text mount once per opening:
// a reopened dialog starts empty without a reset effect.
function TypeToConfirmBody({
  title,
  description,
  expected,
  inputLabel,
  confirmLabel,
  pending,
  error,
  onConfirm,
}: TypeToConfirmProps) {
  const inputId = useId();
  const [typed, setTyped] = useState("");
  const canConfirm = typed === expected && !pending;

  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>{title}</AlertDialogTitle>
        <AlertDialogDescription>{description}</AlertDialogDescription>
      </AlertDialogHeader>
      <div className="flex flex-col gap-1.5">
        <Label htmlFor={inputId}>{inputLabel}</Label>
        <Input
          id={inputId}
          value={typed}
          autoComplete="off"
          spellCheck={false}
          onChange={(event) => {
            setTyped(event.target.value);
          }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              if (canConfirm) {
                onConfirm();
              }
            }
          }}
        />
      </div>
      <FormError message={error} />
      <AlertDialogFooter>
        <AlertDialogCancel disabled={pending}>Cancel</AlertDialogCancel>
        <Button type="button" variant="destructive" disabled={!canConfirm} onClick={onConfirm}>
          {confirmLabel}
        </Button>
      </AlertDialogFooter>
    </>
  );
}
