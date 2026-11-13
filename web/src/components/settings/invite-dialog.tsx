import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { authConfigQuery } from "@/api/auth";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { orgInvitesKey } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
import { CopyButton } from "@/components/copy-button";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { TimeText } from "@/components/time-text";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { grantableRoles, ORG_ROLES, type OrgRole, roleLabel } from "@/lib/roles";

function useCreateInvite(org: string) {
  const queryClient = useQueryClient();
  // gcTime 0: `accept_url` carries the invitation token (D-L-5). `onSuccess` sits on the
  // hook, not on `mutate`, so a close mid-request cannot drop the invalidation.
  return $api.useMutation("post", "/api/v1/orgs/{org}/invites", {
    gcTime: 0,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: orgInvitesKey(org) });
    },
  });
}

type CreateInviteMutation = ReturnType<typeof useCreateInvite>;

export function InviteDialog({
  org,
  actorRole,
  open,
  onOpenChange,
}: {
  org: string;
  actorRole: OrgRole | undefined;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  // The mutation lives here, not in the form: a close mid-request must not lose the link.
  const create = useCreateInvite(org);
  const close = () => {
    create.reset();
    onOpenChange(false);
  };
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // While the request runs, the invite may already exist on the server: closing
        // now would reset the mutation and lose the link. Esc and outside clicks wait.
        if (!next && !create.isPending) {
          close();
        }
      }}
    >
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (create.isPending) {
            event.preventDefault();
          }
        }}
        onPointerDownOutside={(event) => {
          if (create.isPending) {
            event.preventDefault();
          }
        }}
      >
        {open && !create.data ? (
          <InviteForm org={org} actorRole={actorRole} create={create} onCancel={close} />
        ) : null}
        {open && create.data ? <InviteResult created={create.data} onDone={close} /> : null}
      </DialogContent>
    </Dialog>
  );
}

function InviteForm({
  org,
  actorRole,
  create,
  onCancel,
}: {
  org: string;
  actorRole: OrgRole | undefined;
  create: CreateInviteMutation;
  onCancel: () => void;
}) {
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<OrgRole>("member");
  const [emailError, setEmailError] = useState<string | undefined>();
  const [error, setError] = useState<string | undefined>();

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setEmailError(undefined);
    setError(undefined);
    create.reset();
    const trimmed = email.trim();
    if (trimmed.length === 0) {
      setEmailError("Enter an email address.");
      return;
    }
    create.mutate(
      { params: { path: { org } }, body: { email: trimmed, role } },
      {
        onError: (caught) => {
          const status = problemOf(caught)?.status;
          if (status === 409) {
            setEmailError("This person is already a member.");
            return;
          }
          if (status === 403) {
            setError("You can't invite with that role.");
            return;
          }
          const fields = fieldErrors(caught);
          if (fields.email !== undefined) {
            setEmailError(fields.email);
            return;
          }
          if (fields.role !== undefined) {
            setError(fields.role);
            return;
          }
          setError(problemMessage(caught, "Could not create the invitation."));
        },
      },
    );
  }

  return (
    <>
      <DialogHeader>
        <DialogTitle>Invite member</DialogTitle>
        <DialogDescription>They get the role you choose when they accept.</DialogDescription>
      </DialogHeader>
      <form noValidate className="flex flex-col gap-4" onSubmit={onSubmit}>
        <TextField
          id="invite-email"
          type="email"
          label="Email"
          value={email}
          required
          autoComplete="off"
          error={emailError}
          disabled={create.isPending}
          onChange={setEmail}
        />
        <div className="flex flex-col gap-2">
          <Label htmlFor="invite-role">Role</Label>
          <Select
            value={role}
            onValueChange={(value) => {
              const next = ORG_ROLES.find((item) => item === value);
              if (next) {
                setRole(next);
              }
            }}
            disabled={create.isPending}
          >
            <SelectTrigger id="invite-role" className="w-fit">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {grantableRoles(actorRole).map((item) => (
                <SelectItem key={item} value={item}>
                  {roleLabel(item)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <FormError message={error} />
        <DialogFooter>
          <Button type="button" variant="outline" disabled={create.isPending} onClick={onCancel}>
            Cancel
          </Button>
          <SubmitButton pending={create.isPending}>Send invitation</SubmitButton>
        </DialogFooter>
      </form>
    </>
  );
}

function InviteResult({
  created,
  onDone,
}: {
  created: Schema<"CreatedInvite">;
  onDone: () => void;
}) {
  // While the config loads, SMTP counts as off (D-L-7).
  const config = useQuery(authConfigQuery);
  return (
    <>
      <DialogHeader>
        <DialogTitle>Invitation created</DialogTitle>
        <DialogDescription>
          {config.data?.smtp_enabled ? (
            <>
              We emailed an invitation to <strong>{created.invite.email}</strong>. You can also send
              them this link:
            </>
          ) : (
            <>
              Email is not configured on this server, so no email was sent. Send this link to{" "}
              <strong>{created.invite.email}</strong>:
            </>
          )}
        </DialogDescription>
      </DialogHeader>
      <div className="flex items-center gap-2">
        <Input
          aria-label="Invitation link"
          className="font-mono text-xs"
          value={created.accept_url}
          readOnly
        />
        <CopyButton value={created.accept_url} label="invitation link" />
      </div>
      <p className="text-xs text-muted-foreground">
        The link expires <TimeText value={created.invite.expires_at} style="datetime" />.
      </p>
      <DialogFooter>
        <Button type="button" onClick={onDone}>
          Done
        </Button>
      </DialogFooter>
    </>
  );
}
