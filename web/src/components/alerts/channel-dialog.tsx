import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { channelsQuery } from "@/api/queries-alerts";
import type { Schema } from "@/api/types";
import { type HeaderRow, HeadersEditor } from "@/components/alerts/headers-editor";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
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
import { Textarea } from "@/components/ui/textarea";
import { channelKindLabel } from "@/lib/alerts";

const KINDS: readonly Schema<"ChannelKind">[] = ["slack", "webhook", "email"];
/** The form fields a server `400` can name; other fields are not shown. */
const FORM_FIELDS = ["name", "kind", "url", "secret", "headers", "recipients"] as const;

// Module-level so every header row of every open dialog gets a distinct React key.
let headerRowCounter = 0;
function nextHeaderRowId(): number {
  headerRowCounter += 1;
  return headerRowCounter;
}

/** Trimmed names; a later duplicate name wins. Rows with an empty name are dropped. */
function headerMap(rows: HeaderRow[]): Record<string, string> {
  return Object.fromEntries(
    rows.filter((row) => row.name.trim() !== "").map((row) => [row.name.trim(), row.value]),
  );
}

function recipientList(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line !== "");
}

// The response of each holds a secret or a URL: gcTime 0 drops it from the mutation cache once
// reset (L README, "Secrets and the mutation cache").
function useCreateChannel() {
  return $api.useMutation("post", "/api/v1/projects/{project}/channels", { gcTime: 0 });
}

function useUpdateChannel() {
  return $api.useMutation("patch", "/api/v1/projects/{project}/channels/{id}", { gcTime: 0 });
}

type CreateChannel = ReturnType<typeof useCreateChannel>;
type UpdateChannel = ReturnType<typeof useUpdateChannel>;

export function ChannelDialog(props: {
  org: string;
  project: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Absent: create. Present: edit this channel. */
  channel?: Schema<"NotificationChannel">;
}) {
  const { org, project, open, onOpenChange, channel } = props;
  const create = useCreateChannel();
  const update = useUpdateChannel();
  const pending = create.isPending || update.isPending;
  const close = () => {
    create.reset();
    update.reset();
    onOpenChange(false);
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // While a request runs, the channel may already be saved: closing now would
        // reset the mutation. Esc and outside clicks wait.
        if (!next && !pending) {
          close();
        }
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{channel ? "Edit channel" : "New channel"}</DialogTitle>
          <DialogDescription>
            {channel
              ? "Fields you leave empty keep their current value."
              : "Alert rules send their notifications to channels."}
          </DialogDescription>
        </DialogHeader>
        {open ? (
          <ChannelForm
            org={org}
            project={project}
            channel={channel}
            create={create}
            update={update}
            onClose={close}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function ChannelForm({
  org,
  project,
  channel,
  create,
  update,
  onClose,
}: {
  org: string;
  project: string;
  channel: Schema<"NotificationChannel"> | undefined;
  create: CreateChannel;
  update: UpdateChannel;
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const pending = create.isPending || update.isPending;
  const [name, setName] = useState(channel?.name ?? "");
  const [kind, setKind] = useState<Schema<"ChannelKind">>("slack");
  const [url, setUrl] = useState("");
  const [secret, setSecret] = useState("");
  // Edit mode: one row per header name, values unknown (the API never returns them).
  const [headers, setHeaders] = useState<HeaderRow[]>(() =>
    (channel?.header_names ?? []).map((header) => ({
      id: nextHeaderRowId(),
      name: header,
      value: "",
    })),
  );
  const [headersTouched, setHeadersTouched] = useState(false);
  const [recipients, setRecipients] = useState(channel?.recipients.join("\n") ?? "");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | undefined>();

  // In edit mode the kind is fixed (D-K-6).
  const activeKind = channel?.kind ?? kind;

  function invalidate() {
    void queryClient.invalidateQueries({ queryKey: channelsQuery(org, project).queryKey });
  }

  function showError(caught: unknown) {
    const all = fieldErrors(caught);
    const mapped: Record<string, string> = {};
    for (const field of FORM_FIELDS) {
      if (all[field] !== undefined) {
        mapped[field] = all[field];
      }
    }
    setErrors(mapped);
    if (Object.keys(mapped).length > 0) {
      setFormError(undefined);
      return;
    }
    const status = problemOf(caught)?.status;
    if (status === 409) {
      setFormError(problemMessage(caught, "A channel with this name already exists."));
    } else if (status === 403) {
      setFormError("Only project members and above can manage channels.");
    } else {
      setFormError(problemMessage(caught, "Could not save the channel."));
    }
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    const trimmedName = name.trim();
    const trimmedUrl = url.trim();
    const trimmedSecret = secret.trim();
    const recipientValues = recipientList(recipients);

    const found: Record<string, string> = {};
    if (!trimmedName) {
      found.name = "Enter a name.";
    }
    if (!channel) {
      if ((activeKind === "slack" || activeKind === "webhook") && !trimmedUrl) {
        found.url = "Enter a URL.";
      }
      if (activeKind === "webhook" && !trimmedSecret) {
        found.secret = "Enter a signing secret.";
      }
      if (activeKind === "email" && recipientValues.length === 0) {
        found.recipients = "Enter at least one address.";
      }
    }
    setErrors(found);
    setFormError(undefined);
    if (Object.keys(found).length > 0) {
      return;
    }

    const path = { project: projectRef(org, project) };
    if (!channel) {
      const body: Schema<"CreateChannelRequest"> = { name: trimmedName, kind: activeKind };
      if (activeKind === "slack") {
        body.url = trimmedUrl;
      }
      if (activeKind === "webhook") {
        body.url = trimmedUrl;
        body.secret = trimmedSecret;
        body.headers = headerMap(headers);
      }
      if (activeKind === "email") {
        body.recipients = recipientValues;
      }
      create.mutate(
        { params: { path }, body },
        {
          onSuccess: () => {
            invalidate();
            onClose();
          },
          onError: showError,
        },
      );
      return;
    }

    const body: Schema<"UpdateChannelRequest"> = {};
    if (trimmedName !== channel.name) {
      body.name = trimmedName;
    }
    if ((activeKind === "slack" || activeKind === "webhook") && trimmedUrl) {
      body.url = trimmedUrl;
    }
    if (activeKind === "webhook") {
      if (trimmedSecret) {
        body.secret = trimmedSecret;
      }
      if (headersTouched) {
        body.headers = headerMap(headers);
      }
    }
    if (
      activeKind === "email" &&
      JSON.stringify(recipientValues) !== JSON.stringify(channel.recipients)
    ) {
      body.recipients = recipientValues;
    }
    if (Object.keys(body).length === 0) {
      onClose();
      return;
    }
    update.mutate(
      { params: { path: { ...path, id: channel.id } }, body },
      {
        onSuccess: () => {
          invalidate();
          onClose();
        },
        onError: showError,
      },
    );
  }

  const urlDescription = channel
    ? activeKind === "slack"
      ? `Leave empty to keep the current URL (${channel.target_hint}).`
      : "Leave empty to keep the current one."
    : activeKind === "slack"
      ? "The Slack incoming webhook URL (https://hooks.slack.com/…)."
      : undefined;

  return (
    <form noValidate className="flex flex-col gap-4" onSubmit={onSubmit}>
      <TextField
        id="channel-name"
        label="Name"
        value={name}
        maxLength={100}
        error={errors.name}
        disabled={pending}
        onChange={setName}
      />
      {channel ? (
        <p className="text-sm">Kind: {channelKindLabel(channel.kind)}</p>
      ) : (
        <div className="flex flex-col gap-2">
          <Label id="channel-kind-label">Kind</Label>
          <RadioGroup
            aria-labelledby="channel-kind-label"
            value={kind}
            onValueChange={(value) => {
              const next = KINDS.find((item) => item === value);
              if (next) {
                setKind(next);
              }
            }}
            disabled={pending}
          >
            {KINDS.map((item) => (
              <div key={item} className="flex items-center gap-2">
                <RadioGroupItem value={item} id={`channel-kind-${item}`} />
                <Label htmlFor={`channel-kind-${item}`}>{channelKindLabel(item)}</Label>
              </div>
            ))}
          </RadioGroup>
          {errors.kind ? (
            <p id="channel-kind-error" className="text-xs text-destructive">
              {errors.kind}
            </p>
          ) : null}
        </div>
      )}
      {activeKind === "slack" ? (
        <TextField
          id="channel-url"
          label="Webhook URL"
          value={url}
          autoComplete="off"
          description={urlDescription}
          error={errors.url}
          disabled={pending}
          onChange={setUrl}
        />
      ) : null}
      {activeKind === "webhook" ? (
        <>
          <TextField
            id="channel-url"
            label="URL"
            value={url}
            autoComplete="off"
            description={urlDescription}
            error={errors.url}
            disabled={pending}
            onChange={setUrl}
          />
          <TextField
            id="channel-secret"
            label="Signing secret"
            type="password"
            autoComplete="new-password"
            value={secret}
            description={
              channel
                ? "Leave empty to keep the current one."
                : "16 to 256 characters. Verify the X-Llmobs-Signature header with it."
            }
            error={errors.secret}
            disabled={pending}
            onChange={setSecret}
          />
          <HeadersEditor
            rows={headers}
            nextId={nextHeaderRowId}
            description={
              channel
                ? "Values are not shown. Saving replaces every header; re-enter the values."
                : undefined
            }
            error={errors.headers}
            disabled={pending}
            onChange={(rows) => {
              setHeaders(rows);
              setHeadersTouched(true);
            }}
          />
        </>
      ) : null}
      {activeKind === "email" ? (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="channel-recipients">Recipients</Label>
          <Textarea
            id="channel-recipients"
            name="channel-recipients"
            value={recipients}
            disabled={pending}
            aria-invalid={errors.recipients ? true : undefined}
            aria-describedby={
              errors.recipients
                ? "channel-recipients-description channel-recipients-error"
                : "channel-recipients-description"
            }
            onChange={(event) => {
              setRecipients(event.target.value);
            }}
          />
          <p id="channel-recipients-description" className="text-xs text-muted-foreground">
            One address per line, at most 20.
          </p>
          {errors.recipients ? (
            <p id="channel-recipients-error" className="text-xs text-destructive">
              {errors.recipients}
            </p>
          ) : null}
          {!channel ? (
            <p className="text-xs text-muted-foreground">
              Email needs SMTP on the server. Without it the channel cannot be created.
            </p>
          ) : null}
        </div>
      ) : null}
      <FormError message={formError} />
      <DialogFooter>
        <Button type="button" variant="outline" disabled={pending} onClick={onClose}>
          Cancel
        </Button>
        <SubmitButton pending={pending}>{channel ? "Save" : "Create channel"}</SubmitButton>
      </DialogFooter>
    </form>
  );
}
