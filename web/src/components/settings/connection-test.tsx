import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { connectionsKey } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
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

const HINTS: Record<Schema<"TestErrorClass">, string> = {
  pause:
    "The key, quota or model access was refused. Rules that use this connection pause until a test of the default model passes.",
  retryable: "The provider did not answer in time or is overloaded. Try again.",
  permanent: "The request was rejected. Check the model name, base URL and structured output mode.",
};

function useTestConnection() {
  return $api.useMutation("post", "/api/v1/orgs/{org}/llm-connections/{id}/test");
}

type TestConnection = ReturnType<typeof useTestConnection>;

export function ConnectionTest({
  org,
  connection,
  open,
  onOpenChange,
}: {
  org: string;
  connection: Schema<"LlmConnection">;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const test = useTestConnection();

  function close() {
    test.reset();
    onOpenChange(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (next) {
          onOpenChange(true);
          return;
        }
        if (!test.isPending) {
          close();
        }
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Test connection</DialogTitle>
          <DialogDescription>
            Runs one boolean judge call through this connection.
          </DialogDescription>
        </DialogHeader>
        {open ? (
          <ConnectionTestForm org={org} connection={connection} test={test} onClose={close} />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function ConnectionTestForm({
  org,
  connection,
  test,
  onClose,
}: {
  org: string;
  connection: Schema<"LlmConnection">;
  test: TestConnection;
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const [model, setModel] = useState(connection.default_model ?? "");
  const result = test.data;
  const error = test.error
    ? problemOf(test.error)?.status === 429
      ? "Too many tests. Wait a minute."
      : problemMessage(test.error, "Could not run the test.")
    : undefined;

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    const trimmed = model.trim();
    test.mutate(
      {
        params: { path: { org, id: connection.id } },
        body: trimmed ? { model: trimmed } : {},
      },
      {
        onSuccess: () => {
          // The connection's status may have changed.
          void queryClient.invalidateQueries({ queryKey: connectionsKey(org) });
        },
      },
    );
  }

  return (
    <form noValidate className="flex flex-col gap-4" onSubmit={onSubmit}>
      <TextField
        id="connection-test-model"
        label="Model"
        value={model}
        disabled={test.isPending}
        onChange={setModel}
        description="Leave empty to use the connection's default model."
      />
      {result ? (
        result.ok ? (
          <p className="text-sm">
            Connection works ({result.latency_ms} ms, model {result.model})
          </p>
        ) : (
          <div className="flex flex-col gap-1">
            <p className="text-sm font-medium text-destructive">Test failed</p>
            <p className="text-sm">{result.error?.message}</p>
            {result.error ? (
              <p className="text-xs text-muted-foreground">{HINTS[result.error.class]}</p>
            ) : null}
          </div>
        )
      ) : null}
      <FormError message={error} />
      <DialogFooter>
        <Button type="button" variant="outline" disabled={test.isPending} onClick={onClose}>
          Cancel
        </Button>
        <SubmitButton pending={test.isPending}>
          {test.isPending ? "Testing…" : "Run test"}
        </SubmitButton>
      </DialogFooter>
    </form>
  );
}
