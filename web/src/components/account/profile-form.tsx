import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage } from "@/api/problem";
import { meQuery } from "@/api/queries";
import { FormError } from "@/components/form/form-error";
import { FormSuccess } from "@/components/form/form-success";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";

export function ProfileForm() {
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const queryClient = useQueryClient();
  const save = $api.useMutation("patch", "/api/v1/me");
  const [name, setName] = useState(me.user.name ?? "");
  const [saved, setSaved] = useState(false);

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setSaved(false);
    const trimmed = name.trim();
    save.mutate(
      { body: trimmed.length > 0 ? { name: trimmed } : {} },
      {
        onSuccess: () => {
          setName(trimmed);
          setSaved(true);
          void queryClient.invalidateQueries({ queryKey: meQuery.queryKey });
        },
      },
    );
  }

  return (
    <form
      noValidate
      onSubmit={onSubmit}
      className="flex max-w-md flex-col gap-4"
      aria-labelledby="profile-heading"
    >
      <TextField
        id="profile-email"
        label="Email"
        type="email"
        value={me.user.email}
        readOnly
        onChange={() => {}}
        description="Your email can't be changed here."
      />
      <TextField
        id="profile-name"
        label="Display name (optional)"
        value={name}
        maxLength={100}
        autoComplete="name"
        description="Leave empty to show your email instead."
        error={fieldErrors(save.error).name}
        disabled={save.isPending}
        onChange={(value) => {
          setName(value);
          setSaved(false);
        }}
      />
      <FormError
        message={
          save.error && !fieldErrors(save.error).name
            ? problemMessage(save.error, "Could not save your name. Try again.")
            : undefined
        }
      />
      <FormSuccess message={saved ? "Name saved." : undefined} />
      <div>
        <SubmitButton pending={save.isPending}>Save name</SubmitButton>
      </div>
    </form>
  );
}
