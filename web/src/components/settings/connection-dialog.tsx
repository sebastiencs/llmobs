import { useQueryClient } from "@tanstack/react-query";
import { X } from "lucide-react";
import { type FormEvent, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage } from "@/api/problem";
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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const PROVIDERS: { value: Schema<"LlmAdapter">; label: string }[] = [
  { value: "openai", label: "OpenAI-compatible" },
  { value: "anthropic", label: "Anthropic" },
  { value: "gemini", label: "Gemini" },
];

const STRUCTURED_OUTPUT_MODES: { value: Schema<"StructuredOutputMode">; label: string }[] = [
  { value: "native", label: "Native (default)" },
  { value: "tool", label: "Tool call" },
  { value: "prompt", label: "Prompt" },
];

function useCreateConnection() {
  // The request holds the API key and the header values: gcTime 0 drops it from the
  // mutation cache once reset (D-L-5).
  return $api.useMutation("post", "/api/v1/orgs/{org}/llm-connections", { gcTime: 0 });
}

type CreateConnection = ReturnType<typeof useCreateConnection>;

function usePatchConnection() {
  // The same secret rule as the create mutation (D-L-5).
  return $api.useMutation("patch", "/api/v1/orgs/{org}/llm-connections/{id}", { gcTime: 0 });
}

type PatchConnection = ReturnType<typeof usePatchConnection>;

interface HeaderRow {
  id: number;
  name: string;
  value: string;
}

let headerRowId = 0;

function newHeaderRow(name: string): HeaderRow {
  headerRowId += 1;
  return { id: headerRowId, name, value: "" };
}

function adapterValue(value: string): Schema<"LlmAdapter"> {
  return value === "anthropic" || value === "gemini" ? value : "openai";
}

function structuredValue(value: string): Schema<"StructuredOutputMode"> | "" {
  return value === "native" || value === "tool" || value === "prompt" ? value : "";
}

export function ConnectionDialog({
  org,
  connection,
  open,
  onOpenChange,
}: {
  org: string;
  connection?: Schema<"LlmConnection">;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const create = useCreateConnection();
  const patch = usePatchConnection();
  const pending = connection === undefined ? create.isPending : patch.isPending;

  // Unmounting the form on close drops the key and header-value state with it.
  function close() {
    create.reset();
    patch.reset();
    onOpenChange(false);
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        // The key may already be saved on the server while the request runs: closing now
        // would reset the mutation and drop the result. Esc and outside clicks wait.
        if (next) {
          onOpenChange(true);
          return;
        }
        if (!pending) {
          close();
        }
      }}
    >
      <DialogContent
        onEscapeKeyDown={(event) => {
          if (pending) {
            event.preventDefault();
          }
        }}
        onPointerDownOutside={(event) => {
          if (pending) {
            event.preventDefault();
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>{connection ? "Edit connection" : "Add connection"}</DialogTitle>
          <DialogDescription>
            The API key is stored encrypted and never shown again.
          </DialogDescription>
        </DialogHeader>
        {open ? (
          <ConnectionForm
            org={org}
            connection={connection}
            create={create}
            patch={patch}
            onDone={close}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}

function ConnectionForm({
  org,
  connection,
  create,
  patch,
  onDone,
}: {
  org: string;
  connection: Schema<"LlmConnection"> | undefined;
  create: CreateConnection;
  patch: PatchConnection;
  onDone: () => void;
}) {
  const queryClient = useQueryClient();
  const [name, setName] = useState(connection?.name ?? "");
  const [adapter, setAdapter] = useState<Schema<"LlmAdapter">>(connection?.adapter ?? "openai");
  const [apiKey, setApiKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(connection?.base_url ?? "");
  const [defaultModel, setDefaultModel] = useState(connection?.default_model ?? "");
  // "" means the user picked nothing yet: the server default (native) applies.
  const [structured, setStructured] = useState<Schema<"StructuredOutputMode"> | "">(
    connection?.structured_output ?? "",
  );
  const [maxConcurrency, setMaxConcurrency] = useState(
    connection ? String(connection.max_concurrency) : "",
  );
  const [requestsPerMinute, setRequestsPerMinute] = useState(
    connection ? String(connection.requests_per_minute) : "",
  );
  const [rows, setRows] = useState<HeaderRow[]>(() =>
    (connection?.extra_header_names ?? []).map((headerName) => newHeaderRow(headerName)),
  );
  const [headersDirty, setHeadersDirty] = useState(false);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | undefined>();
  const pending = connection === undefined ? create.isPending : patch.isPending;

  function addHeaderRow() {
    setHeadersDirty(true);
    setRows((current) => [...current, newHeaderRow("")]);
  }

  function updateHeaderRow(id: number, change: Partial<HeaderRow>) {
    setHeadersDirty(true);
    setRows((current) => current.map((row) => (row.id === id ? { ...row, ...change } : row)));
  }

  function removeHeaderRow(id: number) {
    setHeadersDirty(true);
    setRows((current) => current.filter((row) => row.id !== id));
  }

  function headerValues(): Record<string, string> {
    const map: Record<string, string> = {};
    for (const row of rows) {
      const rowName = row.name.trim();
      if (rowName.length === 0 && row.value.trim().length === 0) {
        continue;
      }
      map[rowName] = row.value;
    }
    return map;
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setError(undefined);
    setErrors({});
    const trimmedName = name.trim();
    const nextErrors: Record<string, string> = {};
    if (trimmedName.length === 0) {
      nextErrors.name = "Enter a name.";
    }
    if (connection === undefined && apiKey.trim().length === 0) {
      nextErrors.api_key = "Enter the API key.";
    }
    const headers = headersDirty ? headerValues() : undefined;
    if (headers !== undefined) {
      for (const row of rows) {
        const rowName = row.name.trim();
        const filled = rowName.length > 0 || row.value.trim().length > 0;
        if (filled && (rowName.length === 0 || row.value.trim().length === 0)) {
          nextErrors.headers = "Enter a value for each header.";
          break;
        }
      }
    }
    if (Object.keys(nextErrors).length > 0) {
      setErrors(nextErrors);
      return;
    }
    if (connection === undefined) {
      const body: Schema<"CreateLlmConnectionRequest"> = {
        name: trimmedName,
        adapter,
        api_key: apiKey,
      };
      if (baseUrl.trim()) {
        body.base_url = baseUrl.trim();
      }
      if (defaultModel.trim()) {
        body.default_model = defaultModel.trim();
      }
      if (structured !== "") {
        body.structured_output = structured;
      }
      if (maxConcurrency.trim()) {
        body.max_concurrency = Number(maxConcurrency);
      }
      if (requestsPerMinute.trim()) {
        body.requests_per_minute = Number(requestsPerMinute);
      }
      if (headers !== undefined) {
        body.extra_headers = headers;
      }
      create.mutate({ params: { path: { org } }, body }, { onSuccess: done, onError });
      return;
    }
    const body: Schema<"UpdateLlmConnectionRequest"> = {};
    if (trimmedName !== connection.name) {
      body.name = trimmedName;
    }
    if (adapter !== connection.adapter) {
      body.adapter = adapter;
    }
    if (apiKey.trim().length > 0) {
      body.api_key = apiKey;
    }
    if (baseUrl !== (connection.base_url ?? "")) {
      body.base_url = baseUrl;
    }
    if (defaultModel !== (connection.default_model ?? "")) {
      body.default_model = defaultModel;
    }
    if (structured !== "" && structured !== connection.structured_output) {
      body.structured_output = structured;
    }
    if (maxConcurrency !== "" && Number(maxConcurrency) !== connection.max_concurrency) {
      body.max_concurrency = Number(maxConcurrency);
    }
    if (requestsPerMinute !== "" && Number(requestsPerMinute) !== connection.requests_per_minute) {
      body.requests_per_minute = Number(requestsPerMinute);
    }
    if (headers !== undefined) {
      body.extra_headers = headers;
    }
    patch.mutate(
      { params: { path: { org, id: connection.id } }, body },
      { onSuccess: done, onError },
    );
  }

  function done() {
    void queryClient.invalidateQueries({ queryKey: connectionsKey(org) });
    onDone();
  }

  function onError(caught: unknown) {
    const next: Record<string, string> = {};
    let placed = false;
    for (const [field, message] of Object.entries(fieldErrors(caught))) {
      placed = true;
      if (field.startsWith("extra_headers")) {
        if (next.headers === undefined) {
          next.headers = message;
        }
      } else {
        next[field] = message;
      }
    }
    setErrors(next);
    if (placed) {
      return;
    }
    setError(problemMessage(caught, "Could not save the connection."));
  }

  return (
    <form noValidate className="flex flex-col gap-4" onSubmit={onSubmit}>
      <TextField
        id="connection-name"
        label="Name"
        value={name}
        maxLength={100}
        error={errors.name}
        disabled={pending}
        onChange={setName}
      />
      <div className="flex flex-col gap-2">
        <Label htmlFor="connection-provider">Provider</Label>
        <Select
          value={adapter}
          onValueChange={(value) => {
            setAdapter(adapterValue(value));
          }}
          disabled={pending}
        >
          <SelectTrigger id="connection-provider" className="w-fit">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PROVIDERS.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <TextField
        id="connection-api-key"
        label="API key"
        type="password"
        autoComplete="new-password"
        value={apiKey}
        error={errors.api_key}
        disabled={pending}
        placeholder={connection === undefined ? undefined : "Leave empty to keep the current key"}
        onChange={setApiKey}
      />
      <TextField
        id="connection-base-url"
        label="Base URL"
        value={baseUrl}
        error={errors.base_url}
        disabled={pending}
        onChange={setBaseUrl}
        description="For OpenAI-compatible servers such as Azure, vLLM, Ollama, OpenRouter or LiteLLM. Local addresses need the server setting security.allow_private_network_egress."
      />
      <TextField
        id="connection-model"
        label="Default model"
        value={defaultModel}
        error={errors.default_model}
        disabled={pending}
        onChange={setDefaultModel}
      />
      <div className="flex flex-col gap-2">
        <Label htmlFor="connection-structured-output">Structured output</Label>
        <Select
          value={structured}
          onValueChange={(value) => {
            setStructured(structuredValue(value));
          }}
          disabled={pending}
        >
          <SelectTrigger id="connection-structured-output" className="w-fit">
            <SelectValue placeholder="Native (default)" />
          </SelectTrigger>
          <SelectContent>
            {STRUCTURED_OUTPUT_MODES.map((option) => (
              <SelectItem key={option.value} value={option.value}>
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <p className="text-xs text-muted-foreground">
          Use Tool call or Prompt only if the provider rejects native structured output.
        </p>
      </div>
      <details>
        <summary className="cursor-pointer text-sm font-medium">Advanced</summary>
        <div className="mt-4 flex flex-col gap-4">
          <div className="flex flex-col gap-4 sm:flex-row">
            <TextField
              id="connection-max-concurrency"
              type="number"
              label="Max concurrency"
              value={maxConcurrency}
              placeholder="4"
              error={errors.max_concurrency}
              disabled={pending}
              onChange={setMaxConcurrency}
            />
            <TextField
              id="connection-requests-per-minute"
              type="number"
              label="Requests per minute"
              value={requestsPerMinute}
              placeholder="300"
              error={errors.requests_per_minute}
              disabled={pending}
              onChange={setRequestsPerMinute}
            />
          </div>
          <div className="flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <Label>Extra headers</Label>
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={pending}
                onClick={addHeaderRow}
              >
                Add header
              </Button>
            </div>
            {connection !== undefined ? (
              <p className="text-xs text-muted-foreground">
                Values are hidden. Set every header again to change them.
              </p>
            ) : null}
            {rows.map((row) => (
              <div key={row.id} className="flex items-center gap-2">
                <Input
                  aria-label="Header name"
                  value={row.name}
                  disabled={pending}
                  onChange={(event) => updateHeaderRow(row.id, { name: event.target.value })}
                />
                <Input
                  aria-label="Header value"
                  value={row.value}
                  disabled={pending}
                  onChange={(event) => updateHeaderRow(row.id, { value: event.target.value })}
                />
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  aria-label="Remove header"
                  disabled={pending}
                  onClick={() => removeHeaderRow(row.id)}
                >
                  <X />
                </Button>
              </div>
            ))}
            {errors.headers ? <p className="text-xs text-destructive">{errors.headers}</p> : null}
          </div>
        </div>
      </details>
      <FormError message={error} />
      <DialogFooter>
        <Button type="button" variant="outline" disabled={pending} onClick={onDone}>
          Cancel
        </Button>
        <SubmitButton pending={pending}>
          {connection === undefined ? "Add connection" : "Save"}
        </SubmitButton>
      </DialogFooter>
    </form>
  );
}
