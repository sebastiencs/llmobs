import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { X } from "lucide-react";
import { type FormEvent, useEffect, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { evaluatorKey, evaluatorsKey, evaluatorTemplatesInit } from "@/api/queries-evaluation";
import { connectionsInit } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
import { DisabledReason } from "@/components/disabled-reason";
import {
  buildCheckConfig,
  CHECK_LABELS,
  CHECK_NAMES,
  CheckFields,
  type CheckName,
  type CheckParams,
  checkDataType,
  checkStateOf,
  defaultCheckParams,
} from "@/components/evaluators/check-fields";
import {
  buildOutput,
  defaultOutputState,
  OutputFields,
  type OutputState,
  outputStateOfJudge,
  outputStateOfScore,
} from "@/components/evaluators/output-fields";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { useProject } from "@/components/shell/project-context";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { canManageEvaluation } from "@/lib/roles";

type Kind = Schema<"EvaluatorKind">;
type Template = Schema<"EvaluatorTemplate">;

const KIND_LABELS: Record<Kind, string> = {
  llm_judge: "LLM judge",
  check: "Check",
  webhook: "Webhook",
};

const NO_PERMISSION = "Only members can edit evaluators";

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

/** Every value the form edits. `buildParts` turns it into the request. */
interface FormValues {
  kind: Kind;
  name: string;
  connectionId: string;
  model: string;
  prompt: string;
  output: OutputState;
  includeReason: boolean;
  maxOutputTokens: string;
  timeoutMs: string;
  temperature: string;
  check: CheckName;
  checkParams: CheckParams;
  url: string;
  headers: HeaderRow[];
  replaceWebhook: boolean;
  secret: string;
  scoreName: string;
}

function stringOf(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function numberText(value: unknown): string {
  return typeof value === "number" ? String(value) : "";
}

function initialValues(evaluator: Schema<"Evaluator"> | undefined): FormValues {
  const config: Record<string, unknown> = evaluator?.config ?? {};
  const kind = evaluator?.kind ?? "llm_judge";
  const check = checkStateOf(config);
  const headerNames = Array.isArray(config.header_names)
    ? config.header_names.filter((item): item is string => typeof item === "string")
    : [];
  let output = defaultOutputState();
  if (kind === "llm_judge") {
    output = outputStateOfJudge(config.output);
  } else if (evaluator) {
    output = outputStateOfScore(evaluator.output_score);
  }
  return {
    kind,
    name: evaluator?.name ?? "",
    connectionId: stringOf(config.connection_id),
    model: stringOf(config.model),
    prompt: stringOf(config.prompt),
    output,
    includeReason: config.include_reason !== false,
    maxOutputTokens: numberText(config.max_output_tokens),
    timeoutMs: numberText(config.timeout_ms),
    temperature: numberText(config.temperature),
    check: check.check,
    checkParams: check.params,
    url: "",
    headers: headerNames.map((headerName) => newHeaderRow(headerName)),
    replaceWebhook: false,
    secret: "",
    scoreName: evaluator?.output_score.name ?? "",
  };
}

interface Parts {
  name: string;
  /** The kind's `config`. Absent for a webhook that keeps its saved settings. */
  config?: Record<string, unknown>;
  outputScore: Schema<"OutputScoreBody">;
  secret: string;
  /** Client errors, keyed by the control they belong to. */
  errors: Record<string, string>;
}

function utf8Length(text: string): number {
  return new TextEncoder().encode(text).length;
}

/** Whether a signing secret is 16 to 256 characters with no control character (E's rule). */
function secretProblem(secret: string): string | undefined {
  const characters = [...secret];
  const hasControl = characters.some((character) => {
    const code = character.codePointAt(0) ?? 0;
    return code < 0x20 || code === 0x7f;
  });
  if (characters.length < 16 || characters.length > 256 || hasControl) {
    return "Enter a signing secret of 16 to 256 characters.";
  }
  return undefined;
}

/** Turns the form into the request parts of its kind, and the client errors that block a save. */
function buildParts(values: FormValues, saved: boolean): Parts {
  const errors: Record<string, string> = {};
  const name = values.name.trim();
  if (name.length === 0) {
    errors.name = "Enter a name.";
  }
  const scoreName = values.scoreName.trim();
  if (scoreName.length === 0) {
    errors.scoreName = "Enter a score name.";
  } else if (utf8Length(scoreName) > 255) {
    errors.scoreName = "Score name must be 1 to 255 bytes.";
  }

  let config: Record<string, unknown> | undefined;
  let score: Schema<"OutputScoreBody">;
  const secret = values.secret;

  if (values.kind === "llm_judge") {
    if (values.connectionId.length === 0) {
      errors.connection = "Choose a connection.";
    }
    const model = values.model.trim();
    if (model.length === 0) {
      errors.model = "Enter a model.";
    }
    if (values.prompt.trim().length === 0) {
      errors.prompt = "Enter a prompt.";
    }
    const output = buildOutput(values.output, true);
    Object.assign(errors, output.errors);
    const built: Record<string, unknown> = {
      connection_id: values.connectionId,
      model,
      prompt: values.prompt,
      output: output.output,
      include_reason: values.includeReason,
    };
    if (values.maxOutputTokens.trim()) {
      const number = Number(values.maxOutputTokens);
      if (Number.isInteger(number)) {
        built.max_output_tokens = number;
      } else {
        errors.maxOutputTokens = "Enter a whole number.";
      }
    }
    if (values.timeoutMs.trim()) {
      const number = Number(values.timeoutMs);
      if (Number.isInteger(number)) {
        built.timeout_ms = number;
      } else {
        errors.timeout = "Enter a whole number of milliseconds.";
      }
    }
    if (values.temperature.trim()) {
      const number = Number(values.temperature);
      if (Number.isFinite(number)) {
        built.temperature = number;
      } else {
        errors.temperature = "Enter a number.";
      }
    }
    config = built;
    score = { name: scoreName, ...output.score };
  } else if (values.kind === "check") {
    const built = buildCheckConfig(values.check, values.checkParams);
    Object.assign(errors, built.errors);
    config = built.config;
    score = { name: scoreName, data_type: checkDataType(values.check) };
  } else {
    const replacing = !saved || values.replaceWebhook;
    if (replacing) {
      const url = values.url.trim();
      if (url.length === 0) {
        errors.url = "Enter a URL.";
      }
      const webhook: Record<string, unknown> = { url };
      if (values.timeoutMs.trim()) {
        const number = Number(values.timeoutMs);
        if (Number.isInteger(number)) {
          webhook.timeout_ms = number;
        } else {
          errors.timeout = "Enter a whole number of milliseconds.";
        }
      }
      const headers: Record<string, string> = {};
      const seen = new Set<string>();
      for (const row of values.headers) {
        const headerName = row.name.trim();
        if (headerName.length === 0 && row.value.trim().length === 0) {
          continue;
        }
        if (headerName.length === 0 || row.value.trim().length === 0) {
          errors.headers ??= "Enter a name and a value for each header.";
          continue;
        }
        const lowered = headerName.toLowerCase();
        if (seen.has(lowered)) {
          errors.headers ??= "Duplicate header name.";
        }
        seen.add(lowered);
        headers[headerName] = row.value;
      }
      if (Object.keys(headers).length > 0) {
        webhook.headers = headers;
      }
      config = webhook;
    }
    if (!saved || replacing || values.secret.length > 0) {
      const problem = secretProblem(values.secret);
      if (problem !== undefined) {
        errors.secret = problem;
      }
    }
    const output = buildOutput(values.output, false);
    Object.assign(errors, output.errors);
    score = { name: scoreName, ...output.score };
  }

  return { name, config, outputScore: score, secret, errors };
}

/** `{{ name }}` variables of a prompt, for display. The server is the authority. */
function variablesOf(prompt: string): string[] {
  const names = new Set<string>();
  for (const match of prompt.matchAll(/\{\{\s*([A-Za-z_][A-Za-z0-9_]*)/g)) {
    const name = match[1];
    if (name !== undefined) {
      names.add(name);
    }
  }
  return [...names];
}

/** Maps a server field to the form control that shows it. */
function placeField(field: string): string | undefined {
  if (field === "name") {
    return "name";
  }
  if (field === "webhook_secret") {
    return "secret";
  }
  if (field === "output_score.name") {
    return "scoreName";
  }
  if (field === "output_score.data_type") {
    return "dataType";
  }
  if (field === "output_score.categories") {
    return "labels";
  }
  if (field.startsWith("output_score")) {
    return "output";
  }
  const map: Record<string, string> = {
    "config.connection_id": "connection",
    "config.model": "model",
    "config.prompt": "prompt",
    "config.output.min": "min",
    "config.output.max": "max",
    "config.output.labels": "labels",
    "config.output.label_values": "labels",
    "config.url": "url",
    "config.timeout_ms": "timeout",
    "config.max_output_tokens": "maxOutputTokens",
    "config.temperature": "temperature",
    "config.pattern": "pattern",
    "config.schema": "schema",
    "config.values": "values",
  };
  const mapped = map[field];
  if (mapped !== undefined) {
    return mapped;
  }
  if (field.startsWith("config.output")) {
    return "output";
  }
  if (field.startsWith("config.headers")) {
    return "headers";
  }
  if (field === "config" || field.startsWith("config.")) {
    return "config";
  }
  return undefined;
}

function SectionError({ message }: { message: string | undefined }) {
  if (!message) {
    return null;
  }
  return (
    <p role="alert" className="text-xs text-destructive">
      {message}
    </p>
  );
}

export function EvaluatorForm({
  org,
  project,
  evaluator,
  onSaved,
}: {
  org: string;
  project: string;
  evaluator?: Schema<"Evaluator">;
  onSaved: (saved: Schema<"Evaluator">) => void;
}) {
  const { data: projectData } = useProject();
  const queryClient = useQueryClient();
  const canEdit = canManageEvaluation(projectData.role);
  const ref = projectRef(org, project);
  const saved = evaluator !== undefined;
  const [initial] = useState(() => initialValues(evaluator));
  // The baseline is what the saved evaluator sends, so an edit sends only what changed.
  const [baseline] = useState(() => buildParts(initial, true));
  const [values, setValues] = useState<FormValues>(initial);
  const [templateId, setTemplateId] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [formError, setFormError] = useState<string | undefined>();
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const create = $api.useMutation("post", "/api/v1/projects/{project}/evaluators", { gcTime: 0 });
  const patch = $api.useMutation("patch", "/api/v1/projects/{project}/evaluators/{id}", {
    gcTime: 0,
  });
  const templates = $api.useQuery(
    "get",
    "/api/v1/projects/{project}/evaluator-templates",
    evaluatorTemplatesInit(org, project),
    { enabled: !saved },
  );
  const connections = $api.useQuery(
    "get",
    "/api/v1/orgs/{org}/llm-connections",
    connectionsInit(org),
  );
  const pending = create.isPending || patch.isPending;
  const locked = pending || !canEdit;

  // Secrets live only in this form's state: drop the mutation results when it unmounts (D-L-5).
  const resetCreate = create.reset;
  const resetPatch = patch.reset;
  useEffect(
    () => () => {
      resetCreate();
      resetPatch();
    },
    [resetCreate, resetPatch],
  );

  function set(change: Partial<FormValues>) {
    setValues((current) => ({ ...current, ...change }));
  }

  function pickTemplate(template: Template) {
    setTemplateId(template.id);
    set({
      name: template.name,
      prompt: template.prompt,
      output: outputStateOfJudge(template.output),
      scoreName: template.output_score_name,
    });
  }

  function pickConnection(id: string) {
    const connection = connections.data?.data.find((item) => item.id === id);
    set({
      connectionId: id,
      model:
        values.model.trim().length === 0 && connection?.default_model
          ? connection.default_model
          : values.model,
    });
  }

  function onError(caught: unknown) {
    const placed: Record<string, string> = {};
    const unplaced: string[] = [];
    for (const [field, message] of Object.entries(fieldErrors(caught))) {
      const key = placeField(field);
      if (key === undefined) {
        unplaced.push(message);
      } else {
        placed[key] ??= message;
      }
    }
    setErrors(placed);
    if (unplaced.length > 0) {
      setFormError(unplaced.join(" "));
    } else if (Object.keys(placed).length === 0) {
      setFormError(problemMessage(caught, "Could not save the evaluator."));
    }
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    setFormError(undefined);
    const parts = buildParts(values, saved);
    if (Object.keys(parts.errors).length > 0) {
      setErrors(parts.errors);
      return;
    }
    setErrors({});
    if (evaluator === undefined) {
      const body: Schema<"CreateEvaluatorRequest"> = {
        name: parts.name,
        kind: values.kind,
        config: configBody(parts.config),
        output_score: parts.outputScore,
        ...(values.kind === "webhook" ? { webhook_secret: parts.secret } : {}),
      };
      create.mutate(
        { params: { path: { project: ref } }, body },
        {
          onSuccess: (created) => {
            void queryClient.invalidateQueries({ queryKey: evaluatorsKey(org, project) });
            onSaved(created);
          },
          onError,
        },
      );
      return;
    }
    // Only changed fields are sent. A changed field inside `config` sends the whole config.
    const body: Schema<"UpdateEvaluatorRequest"> = {};
    if (parts.name !== baseline.name) {
      body.name = parts.name;
    }
    if (
      parts.config !== undefined &&
      JSON.stringify(parts.config) !== JSON.stringify(baseline.config)
    ) {
      body.config = configBody(parts.config);
    }
    if (JSON.stringify(parts.outputScore) !== JSON.stringify(baseline.outputScore)) {
      body.output_score = parts.outputScore;
    }
    if (values.kind === "webhook" && parts.secret.length > 0) {
      body.webhook_secret = parts.secret;
    }
    patch.mutate(
      { params: { path: { project: ref, id: evaluator.id } }, body },
      {
        onSuccess: (updated) => {
          void queryClient.invalidateQueries({ queryKey: evaluatorsKey(org, project) });
          void queryClient.invalidateQueries({ queryKey: evaluatorKey(org, project, updated.id) });
          onSaved(updated);
        },
        onError,
      },
    );
  }

  const advancedError =
    errors.maxOutputTokens !== undefined ||
    errors.timeout !== undefined ||
    errors.temperature !== undefined;
  const connectionList = connections.data?.data ?? [];
  const templateList = templates.data?.data ?? [];
  const variables = variablesOf(values.prompt);
  const webhookSettingsShown = !saved || values.replaceWebhook;
  const config: Record<string, unknown> = evaluator?.config ?? {};
  const headerNames = Array.isArray(config.header_names)
    ? config.header_names.filter((item): item is string => typeof item === "string")
    : [];

  return (
    <form noValidate className="flex flex-col gap-6" onSubmit={onSubmit}>
      <fieldset disabled={locked} className="flex min-w-0 flex-col gap-6">
        {!saved && values.kind === "llm_judge" ? (
          <div className="flex flex-col gap-2">
            <Label htmlFor="evaluator-template">Start from a template</Label>
            <Select
              value={templateId}
              onValueChange={(id) => {
                const template = templateList.find((item) => item.id === id);
                if (template) {
                  pickTemplate(template);
                }
              }}
            >
              <SelectTrigger id="evaluator-template" className="w-fit">
                <SelectValue placeholder="Choose a template" />
              </SelectTrigger>
              <SelectContent>
                {templateList.map((template) => (
                  <SelectItem key={template.id} value={template.id}>
                    {template.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">Replaces the name, prompt and output.</p>
          </div>
        ) : null}

        <TextField
          id="evaluator-name"
          label="Name"
          value={values.name}
          maxLength={128}
          error={errors.name}
          onChange={(name) => {
            set({ name });
          }}
        />

        {saved ? (
          <p className="text-sm text-muted-foreground">Kind: {KIND_LABELS[values.kind]}</p>
        ) : (
          <div className="flex flex-col gap-2">
            <span className="text-sm leading-none font-medium">Kind</span>
            <RadioGroup
              aria-label="Kind"
              value={values.kind}
              onValueChange={(next) => {
                set({ kind: kindOf(next) });
              }}
              className="flex-row gap-4"
            >
              {(["llm_judge", "check", "webhook"] as const).map((item) => (
                <div key={item} className="flex items-center gap-2">
                  <RadioGroupItem value={item} id={`evaluator-kind-${item}`} />
                  <Label htmlFor={`evaluator-kind-${item}`}>{KIND_LABELS[item]}</Label>
                </div>
              ))}
            </RadioGroup>
          </div>
        )}

        {values.kind === "llm_judge" ? (
          <section className="flex flex-col gap-6" aria-label="Judge">
            <div className="flex flex-col gap-2">
              <Label htmlFor="evaluator-connection">Connection</Label>
              <Select value={values.connectionId} onValueChange={pickConnection}>
                <SelectTrigger
                  id="evaluator-connection"
                  className="w-fit"
                  aria-invalid={errors.connection ? true : undefined}
                >
                  <SelectValue placeholder="Choose a connection" />
                </SelectTrigger>
                <SelectContent>
                  {connectionList.map((connection) => (
                    <SelectItem key={connection.id} value={connection.id}>
                      {connection.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {connections.isSuccess && connectionList.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  No connections. An org admin adds them in{" "}
                  <Link
                    to="/$org/settings/llm-connections"
                    params={{ org }}
                    className="underline underline-offset-4"
                  >
                    Settings › LLM connections
                  </Link>
                  .
                </p>
              ) : null}
              <SectionError message={errors.connection} />
            </div>
            <TextField
              id="evaluator-model"
              label="Model"
              value={values.model}
              error={errors.model}
              onChange={(model) => {
                set({ model });
              }}
            />
            <div className="flex flex-col gap-2">
              <Label htmlFor="evaluator-prompt">Prompt</Label>
              <Textarea
                id="evaluator-prompt"
                className="font-mono"
                rows={16}
                value={values.prompt}
                aria-invalid={errors.prompt ? true : undefined}
                aria-describedby="evaluator-prompt-variables"
                onChange={(event) => {
                  set({ prompt: event.target.value });
                }}
              />
              <p id="evaluator-prompt-variables" className="text-xs text-muted-foreground">
                {variables.length > 0 ? `Variables: ${variables.join(", ")}` : "No variables."}
              </p>
              <SectionError message={errors.prompt} />
            </div>
            <div className="flex flex-col gap-2">
              <span className="text-sm leading-none font-medium">Output</span>
              <OutputFields
                judge
                value={values.output}
                errors={errors}
                onChange={(output) => {
                  set({ output });
                }}
              />
              <SectionError message={errors.output ?? errors.dataType} />
            </div>
            <div className="flex items-center gap-2">
              <Switch
                id="evaluator-include-reason"
                size="sm"
                checked={values.includeReason}
                onCheckedChange={(includeReason) => {
                  set({ includeReason });
                }}
              />
              <Label htmlFor="evaluator-include-reason">Include reason</Label>
            </div>
            <details
              open={advancedOpen || advancedError}
              onToggle={(event) => {
                setAdvancedOpen(event.currentTarget.open);
              }}
            >
              <summary className="cursor-pointer text-sm font-medium">Advanced</summary>
              <div className="mt-4 flex flex-col gap-4">
                <div className="flex flex-col gap-4 sm:flex-row">
                  <TextField
                    id="evaluator-max-output-tokens"
                    type="number"
                    label="Max output tokens"
                    value={values.maxOutputTokens}
                    placeholder="1024"
                    error={errors.maxOutputTokens}
                    onChange={(maxOutputTokens) => {
                      set({ maxOutputTokens });
                    }}
                  />
                  <TextField
                    id="evaluator-timeout"
                    type="number"
                    label="Timeout ms"
                    value={values.timeoutMs}
                    placeholder="60000"
                    error={errors.timeout}
                    onChange={(timeoutMs) => {
                      set({ timeoutMs });
                    }}
                  />
                  <TextField
                    id="evaluator-temperature"
                    type="number"
                    label="Temperature"
                    value={values.temperature}
                    placeholder="Provider default"
                    error={errors.temperature}
                    onChange={(temperature) => {
                      set({ temperature });
                    }}
                  />
                </div>
                <SectionError message={errors.config} />
              </div>
            </details>
          </section>
        ) : null}

        {values.kind === "check" ? (
          <section className="flex flex-col gap-4" aria-label="Check">
            <div className="flex flex-col gap-2">
              <Label htmlFor="evaluator-check">Check</Label>
              <Select
                value={values.check}
                onValueChange={(next) => {
                  const check = checkOf(next);
                  set({ check, checkParams: defaultCheckParams(check) });
                }}
              >
                <SelectTrigger id="evaluator-check" className="w-fit">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {CHECK_NAMES.map((item) => (
                    <SelectItem key={item} value={item}>
                      {CHECK_LABELS[item]}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <CheckFields
              check={values.check}
              value={values.checkParams}
              errors={errors}
              onChange={(checkParams) => {
                set({ checkParams });
              }}
            />
            <p className="text-sm text-muted-foreground">
              Output: {checkDataType(values.check) === "numeric" ? "Numeric" : "Boolean"}
            </p>
            <SectionError message={errors.config} />
          </section>
        ) : null}

        {values.kind === "webhook" ? (
          <section className="flex flex-col gap-4" aria-label="Webhook">
            {saved ? (
              <div className="flex flex-col gap-2 text-sm">
                <p>Endpoint host: {stringOf(config.url_host) || "—"}</p>
                <p>Headers: {headerNames.length > 0 ? headerNames.join(", ") : "none"}</p>
                <div className="flex items-center gap-2">
                  <Checkbox
                    id="evaluator-replace-webhook"
                    checked={values.replaceWebhook}
                    onCheckedChange={(checked) => {
                      set({ replaceWebhook: checked === true });
                    }}
                  />
                  <Label htmlFor="evaluator-replace-webhook">Replace webhook settings</Label>
                </div>
              </div>
            ) : null}
            {webhookSettingsShown ? (
              <>
                <TextField
                  id="evaluator-url"
                  label="URL"
                  value={values.url}
                  error={errors.url}
                  onChange={(url) => {
                    set({ url });
                  }}
                />
                <TextField
                  id="evaluator-webhook-timeout"
                  type="number"
                  label="Timeout ms"
                  value={values.timeoutMs}
                  placeholder="10000"
                  error={errors.timeout}
                  onChange={(timeoutMs) => {
                    set({ timeoutMs });
                  }}
                />
                <fieldset className="flex flex-col gap-2">
                  <legend className="text-sm leading-none font-medium">Headers</legend>
                  <div className="flex items-center justify-end gap-2">
                    {saved ? (
                      <p className="mr-auto text-xs text-muted-foreground">
                        Values are hidden. Set every header again to change them.
                      </p>
                    ) : null}
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      onClick={() => {
                        set({ headers: [...values.headers, newHeaderRow("")] });
                      }}
                    >
                      Add header
                    </Button>
                  </div>
                  {values.headers.map((row, index) => (
                    <div key={row.id} className="flex items-center gap-2">
                      <Input
                        aria-label={`Header name ${index + 1}`}
                        aria-invalid={errors.headers ? true : undefined}
                        value={row.name}
                        onChange={(event) => {
                          const name = event.target.value;
                          set({
                            headers: values.headers.map((item) =>
                              item.id === row.id ? { ...item, name } : item,
                            ),
                          });
                        }}
                      />
                      <Input
                        aria-label={`Header value ${index + 1}`}
                        aria-invalid={errors.headers ? true : undefined}
                        type="password"
                        autoComplete="off"
                        value={row.value}
                        onChange={(event) => {
                          const value = event.target.value;
                          set({
                            headers: values.headers.map((item) =>
                              item.id === row.id ? { ...item, value } : item,
                            ),
                          });
                        }}
                      />
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon-xs"
                        aria-label={`Remove header ${index + 1}`}
                        onClick={() => {
                          set({ headers: values.headers.filter((item) => item.id !== row.id) });
                        }}
                      >
                        <X />
                      </Button>
                    </div>
                  ))}
                  <SectionError message={errors.headers} />
                </fieldset>
              </>
            ) : null}
            <TextField
              id="evaluator-secret"
              label="Signing secret"
              type="password"
              autoComplete="new-password"
              value={values.secret}
              error={errors.secret}
              placeholder={saved ? "Leave empty to keep the current secret" : undefined}
              onChange={(secret) => {
                set({ secret });
              }}
            />
            <div className="flex flex-col gap-2">
              <span className="text-sm leading-none font-medium">Output</span>
              <OutputFields
                judge={false}
                value={values.output}
                errors={errors}
                onChange={(output) => {
                  set({ output });
                }}
              />
              <SectionError message={errors.output ?? errors.dataType} />
            </div>
            <SectionError message={errors.config} />
          </section>
        ) : null}

        <TextField
          id="evaluator-score-name"
          label="Score name"
          value={values.scoreName}
          error={errors.scoreName}
          description="The score this evaluator writes. Its definition, if one exists, is checked on every write."
          onChange={(scoreName) => {
            set({ scoreName });
          }}
        />

        <FormError message={formError} />
        <div className="flex justify-end">
          <DisabledReason reason={canEdit ? undefined : NO_PERMISSION}>
            <SubmitButton pending={pending}>{saved ? "Save" : "Create evaluator"}</SubmitButton>
          </DisabledReason>
        </div>
      </fieldset>
    </form>
  );
}

/**
 * The generated type of a `config` object is `Record<string, never>` (the schema says
 * `value_type = Object`). The kind-specific shape is checked by the server, not by this type.
 */
function configBody(
  config: Record<string, unknown> | undefined,
): Schema<"CreateEvaluatorRequest">["config"] {
  return (config ?? {}) as Schema<"CreateEvaluatorRequest">["config"];
}

function kindOf(value: string): Kind {
  return value === "check" || value === "webhook" ? value : "llm_judge";
}

function checkOf(value: string): CheckName {
  return CHECK_NAMES.find((name) => name === value) ?? "exact_match";
}
