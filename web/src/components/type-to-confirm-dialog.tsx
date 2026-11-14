import type { ReactNode } from "react";
import { useEffect, useId, useRef, useState } from "react";
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

/** A destructive confirmation that needs the exact text (a slug or a user id) typed first. */
export function TypeToConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  expected,
  inputLabel,
  confirmLabel,
  pending,
  error,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
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
}) {
  const inputId = useId();
  const inputRef = useRef<HTMLInputElement>(null);
  const [typed, setTyped] = useState("");
  const canConfirm = typed === expected && !pending;

  // A reopened dialog starts empty, so a confirmation never carries over.
  useEffect(() => {
    if (open) {
      setTyped("");
    }
  }, [open]);

  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        // Esc and the outside click must not close the dialog while the request runs.
        if (!pending) {
          onOpenChange(next);
        }
      }}
    >
      <AlertDialogContent
        onOpenAutoFocus={(event) => {
          // Focus the input, not Cancel, so the user can type straight away.
          event.preventDefault();
          inputRef.current?.focus();
        }}
      >
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description}</AlertDialogDescription>
        </AlertDialogHeader>
        <div className="flex flex-col gap-1.5">
          <Label htmlFor={inputId}>{inputLabel}</Label>
          <Input
            ref={inputRef}
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
      </AlertDialogContent>
    </AlertDialog>
  );
}
