import { useState } from "react";
import { CopyButton } from "@/components/copy-button";
import { SnippetTabs } from "@/components/onboarding/snippet-tabs";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

type SecretDialogProps = {
  open: boolean;
  title: string;
  secret: string;
  description?: string;
  showSnippets?: boolean;
  onDone: () => void;
};

/** D-L-5's one-time secret dialog. The key remounts the body when the secret changes, so `copied` and `confirmingClose` start fresh (README § Pitfalls). */
export function SecretDialog(props: SecretDialogProps) {
  return <SecretDialogBody key={props.secret} {...props} />;
}

function SecretDialogBody({
  open,
  title,
  secret,
  description,
  showSnippets,
  onDone,
}: SecretDialogProps) {
  const [copied, setCopied] = useState(false);
  const [confirmingClose, setConfirmingClose] = useState(false);

  function finish() {
    setCopied(false);
    setConfirmingClose(false);
    onDone();
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          finish();
        }
      }}
    >
      <DialogContent
        showCloseButton={false}
        className={showSnippets ? "sm:max-w-2xl" : undefined}
        onEscapeKeyDown={(event) => {
          if (!copied) {
            event.preventDefault();
            setConfirmingClose(true);
          }
        }}
        onInteractOutside={(event) => {
          if (!copied) {
            event.preventDefault();
            setConfirmingClose(true);
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>
            {description ? `${description} ` : ""}Copy it now. You won't see it again.
          </DialogDescription>
        </DialogHeader>
        <div className="flex min-w-0 flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="secret-value">Secret</Label>
            <div className="flex items-center gap-2">
              <Input
                id="secret-value"
                value={secret}
                readOnly
                className="font-mono text-xs"
                onFocus={(event) => {
                  event.currentTarget.select();
                }}
              />
              <CopyButton value={secret} label="secret" onCopied={() => setCopied(true)} />
            </div>
          </div>
          {showSnippets && secret.startsWith("sk-") ? (
            <>
              <p className="text-sm">Send traces with this key:</p>
              <SnippetTabs origin={window.location.origin} apiKey={secret} />
            </>
          ) : null}
          {confirmingClose ? (
            <div
              role="alert"
              className="flex flex-col gap-2 rounded-md border border-destructive/40 p-3 text-sm"
            >
              <p>You haven't copied the secret. Close without copying?</p>
              <div className="flex gap-2">
                <Button type="button" variant="destructive" onClick={finish}>
                  Close anyway
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  onClick={() => {
                    setConfirmingClose(false);
                  }}
                >
                  Go back
                </Button>
              </div>
            </div>
          ) : null}
        </div>
        <DialogFooter>
          <Button type="button" onClick={finish}>
            Done
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
