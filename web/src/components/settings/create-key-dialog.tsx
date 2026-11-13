import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { apiKeysKey } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { SecretDialog } from "@/components/secret-dialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const EXPIRY = [
  { value: "never", label: "Never", days: undefined },
  { value: "30", label: "In 30 days", days: 30 },
  { value: "90", label: "In 90 days", days: 90 },
  { value: "365", label: "In 1 year", days: 365 },
] as const;

/** RFC 3339 for `now + days`. */
export function expiresAt(now: number, days: number): string {
  return new Date(now + days * 86_400_000).toISOString();
}

function useCreateKey() {
  // The response holds the secret: gcTime 0 drops it from the mutation cache once reset (D-L-5).
  return $api.useMutation("post", "/api/v1/projects/{project}/api-keys", { gcTime: 0 });
}

type CreateMutation = ReturnType<typeof useCreateKey>;

export function CreateKeyDialog({
  org,
  project,
  open,
  onOpenChange,
}: {
  org: string;
  project: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const create = useCreateKey();
  const close = () => {
    create.reset();
    onOpenChange(false);
  };
  return (
    <>
      <Dialog
        open={open && !create.data}
        onOpenChange={(next) => {
          // While the request runs, the key may already exist on the server: closing now
          // would reset the mutation and lose the secret. Esc and outside clicks wait.
          if (!next && !create.isPending) close();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Create API key</DialogTitle>
            <DialogDescription>Choose who uses the key and how long it works.</DialogDescription>
          </DialogHeader>
          {open && !create.data ? (
            <CreateKeyForm org={org} project={project} create={create} onCancel={close} />
          ) : null}
        </DialogContent>
      </Dialog>
      {create.data ? (
        <SecretDialog
          open
          title={create.data.key.kind === "secret" ? "Secret key created" : "Public key created"}
          secret={create.data.secret}
          description={
            create.data.key.kind === "secret"
              ? "Use it as the bearer token of your SDK or exporter."
              : "Use it in browsers to send end-user feedback."
          }
          showSnippets={create.data.key.kind === "secret"}
          onDone={close}
        />
      ) : null}
    </>
  );
}

function CreateKeyForm({
  org,
  project,
  create,
  onCancel,
}: {
  org: string;
  project: string;
  create: CreateMutation;
  onCancel: () => void;
}) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [kind, setKind] = useState<"secret" | "public">("secret");
  const [scopes, setScopes] = useState<"full" | "ingest">("full");
  const [expiry, setExpiry] = useState<(typeof EXPIRY)[number]["value"]>("never");
  const [nameError, setNameError] = useState<string | undefined>();
  const [expiryError, setExpiryError] = useState<string | undefined>();
  const [error, setError] = useState<string | undefined>();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setNameError(undefined);
    setExpiryError(undefined);
    setError(undefined);
    create.reset();
    const body: Schema<"CreateApiKeyRequest"> = { kind };
    const trimmed = name.trim();
    if (trimmed) {
      body.name = trimmed;
    }
    if (kind === "secret" && scopes === "ingest") {
      body.scopes = ["ingest"];
    }
    const days = EXPIRY.find((item) => item.value === expiry)?.days;
    if (days !== undefined) {
      body.expires_at = expiresAt(Date.now(), days);
    }
    create.mutate(
      { params: { path: { project: projectRef(org, project) } }, body },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: apiKeysKey(org, project) });
        },
        onError: (caught) => {
          const fields = fieldErrors(caught);
          setNameError(fields.name);
          setExpiryError(fields.expires_at);
          if (fields.name !== undefined || fields.expires_at !== undefined) {
            return;
          }
          if (problemOf(caught)?.status === 403) {
            setError("Only project admins can create keys.");
            return;
          }
          setError(problemMessage(caught, "Could not create the key."));
        },
      },
    );
  }

  return (
    <form noValidate className="flex flex-col gap-4" onSubmit={onSubmit}>
      <TextField
        id="key-name"
        label="Name (optional)"
        value={name}
        maxLength={100}
        description="For example the service or environment that uses it."
        error={nameError}
        disabled={create.isPending}
        onChange={setName}
      />
      <div className="flex flex-col gap-2">
        <Label id="key-kind-label">Kind</Label>
        <RadioGroup
          aria-labelledby="key-kind-label"
          value={kind}
          onValueChange={(value) => {
            setKind(value === "public" ? "public" : "secret");
          }}
          disabled={create.isPending}
        >
          <div className="flex items-start gap-2">
            <RadioGroupItem
              value="secret"
              id="key-kind-secret"
              aria-describedby="key-kind-secret-help"
            />
            <div className="flex flex-col gap-0.5">
              <Label htmlFor="key-kind-secret">Secret key</Label>
              <p id="key-kind-secret-help" className="text-xs text-muted-foreground">
                For servers: SDKs, OpenTelemetry exporters, CI.
              </p>
            </div>
          </div>
          <div className="flex items-start gap-2">
            <RadioGroupItem
              value="public"
              id="key-kind-public"
              aria-describedby="key-kind-public-help"
            />
            <div className="flex flex-col gap-0.5">
              <Label htmlFor="key-kind-public">Public key</Label>
              <p id="key-kind-public-help" className="text-xs text-muted-foreground">
                For browsers: end-user feedback only.
              </p>
            </div>
          </div>
        </RadioGroup>
      </div>
      {kind === "secret" ? (
        <div className="flex flex-col gap-2">
          <Label id="key-scopes-label">Scopes</Label>
          <RadioGroup
            aria-labelledby="key-scopes-label"
            value={scopes}
            onValueChange={(value) => {
              setScopes(value === "ingest" ? "ingest" : "full");
            }}
            disabled={create.isPending}
          >
            <div className="flex items-center gap-2">
              <RadioGroupItem value="full" id="key-scopes-full" />
              <Label htmlFor="key-scopes-full">Ingest, read and write</Label>
            </div>
            <div className="flex items-center gap-2">
              <RadioGroupItem value="ingest" id="key-scopes-ingest" />
              <Label htmlFor="key-scopes-ingest">Ingest only</Label>
            </div>
          </RadioGroup>
        </div>
      ) : null}
      <div className="flex flex-col gap-2">
        <Label htmlFor="key-expiry">Expires</Label>
        <Select
          value={expiry}
          onValueChange={(value) => {
            const next = EXPIRY.find((item) => item.value === value);
            if (next) {
              setExpiry(next.value);
            }
          }}
          disabled={create.isPending}
        >
          <SelectTrigger
            id="key-expiry"
            aria-describedby={expiryError ? "key-expiry-error" : undefined}
            className="w-fit"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {EXPIRY.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {expiryError ? (
          <p id="key-expiry-error" className="text-xs text-destructive">
            {expiryError}
          </p>
        ) : null}
      </div>
      <FormError message={error} />
      <DialogFooter>
        <Button type="button" variant="outline" disabled={create.isPending} onClick={onCancel}>
          Cancel
        </Button>
        <SubmitButton pending={create.isPending}>Create key</SubmitButton>
      </DialogFooter>
    </form>
  );
}
