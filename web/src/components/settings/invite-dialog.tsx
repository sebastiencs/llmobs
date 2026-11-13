import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { authConfigQuery } from "@/api/auth";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { orgInvitesKey } from "@/api/queries-settings";
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
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        {open ? (
          <InviteForm org={org} actorRole={actorRole} onDone={() => onOpenChange(false)} />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function InviteForm({
  org,
  actorRole,
  onDone,
}: {
  org: string;
  actorRole: OrgRole | undefined;
  onDone: () => void;
}) {
  const queryClient = useQueryClient();
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<OrgRole>("member");
  const [emailError, setEmailError] = useState<string | undefined>();
  const [error, setError] = useState<string | undefined>();
  // While the config loads, SMTP counts as off (D-L-7).
  const config = useQuery(authConfigQuery);
  // gcTime 0: `accept_url` carries the invitation token (D-L-5).
  const mutation = $api.useMutation("post", "/api/v1/orgs/{org}/invites", { gcTime: 0 });

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setEmailError(undefined);
    setError(undefined);
    mutation.reset();
    const trimmed = email.trim();
    if (trimmed.length === 0) {
      setEmailError("Enter an email address.");
      return;
    }
    mutation.mutate(
      { params: { path: { org } }, body: { email: trimmed, role } },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: orgInvitesKey(org) });
        },
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

  if (mutation.data) {
    return (
      <>
        <DialogHeader>
          <DialogTitle>Invitation created</DialogTitle>
          <DialogDescription>
            {config.data?.smtp_enabled ? (
              <>
                We emailed an invitation to <strong>{mutation.data.invite.email}</strong>. You can
                also send them this link:
              </>
            ) : (
              <>
                Email is not configured on this server, so no email was sent. Send this link to{" "}
                <strong>{mutation.data.invite.email}</strong>:
              </>
            )}
          </DialogDescription>
        </DialogHeader>
        <div className="flex items-center gap-2">
          <Input
            aria-label="Invitation link"
            className="font-mono text-xs"
            value={mutation.data.accept_url}
            readOnly
          />
          <CopyButton value={mutation.data.accept_url} label="invitation link" />
        </div>
        <p className="text-xs text-muted-foreground">
          The link expires <TimeText value={mutation.data.invite.expires_at} style="datetime" />.
        </p>
        <DialogFooter>
          <Button type="button" onClick={onDone}>
            Done
          </Button>
        </DialogFooter>
      </>
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
          disabled={mutation.isPending}
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
            disabled={mutation.isPending}
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
          <Button type="button" variant="outline" disabled={mutation.isPending} onClick={onDone}>
            Cancel
          </Button>
          <SubmitButton pending={mutation.isPending}>Send invitation</SubmitButton>
        </DialogFooter>
      </form>
    </>
  );
}
