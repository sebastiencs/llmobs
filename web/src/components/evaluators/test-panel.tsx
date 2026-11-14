import { Link } from "@tanstack/react-router";
import { X } from "lucide-react";
import { type FormEvent, useEffect, useState } from "react";
import { $api } from "@/api/client";
import { fieldErrors, problemMessage, problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import type { Schema } from "@/api/types";
import { FormError } from "@/components/form/form-error";
import { SubmitButton } from "@/components/form/submit-button";
import { TextField } from "@/components/form/text-field";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

type Mapping = Schema<"TemplateMapping">;
type Body = Schema<"TestEvaluatorBody">;

const HINTS: Record<Schema<"TestErrorClass">, string> = {
  pause:
    "The key, quota or model access was refused. Check the connection in Settings › LLM connections.",
  retryable: "The provider or webhook did not answer in time or is overloaded. Try again.",
  permanent:
    "The request was rejected or the answer did not fit. Check the model, the prompt and the output type.",
};

const TRACE_ID = /^[0-9a-fA-F]{32}$/;
const SPAN_ID = /^[0-9a-fA-F]{16}$/;

interface MappingRow {
  id: number;
  variable: string;
  expression: string;
}

interface VariableRow {
  id: number;
  name: string;
  value: string;
}

let rowId = 0;

function newMappingRow(variable: string, expression: string): MappingRow {
  rowId += 1;
  return { id: rowId, variable, expression };
}

function newVariableRow(): VariableRow {
  rowId += 1;
  return { id: rowId, name: "", value: "" };
}

/** The prompt of a body. The body is `null` while the form has client errors. */
function promptOf(body: Body | null): string {
  const prompt = (body?.config as Record<string, unknown> | undefined)?.prompt;
  return typeof prompt === "string" ? prompt : "";
}

/** `{{ name }}` variables of a prompt, as in evaluator-form.tsx. */
function variableNames(prompt: string): string[] {
  const names = new Set<string>();
  for (const match of prompt.matchAll(/\{\{\s*([A-Za-z_][A-Za-z0-9_]*)/g)) {
    const name = match[1];
    if (name !== undefined) {
      names.add(name);
    }
  }
  return [...names];
}

/** The usual expression of a prompt variable; other variables start empty. */
function expressionFor(variable: string): string {
  if (variable === "input") {
    return "trace.input";
  }
  if (variable === "output") {
    return "trace.output";
  }
  return "";
}

function initialMapping(defaults: Mapping[], prompt: string): MappingRow[] {
  if (defaults.length > 0) {
    return defaults.map((row) => newMappingRow(row.variable, row.expression));
  }
  return variableNames(prompt).map((variable) => newMappingRow(variable, expressionFor(variable)));
}

/** A variable value that parses as JSON is sent as that JSON, otherwise as a string. */
function parseValue(text: string): unknown {
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return text;
  }
}

/** The fields of the run request that depend on the mode, and the client errors that block it. */
interface RunFields {
  fields: Record<string, unknown>;
  errors: Record<string, string>;
}

function formatValue(value: Schema<"ScoreValue">): string {
  switch (value.data_type) {
    case "numeric":
      return value.value.toFixed(3);
    case "boolean":
      return value.value ? "true" : "false";
    case "categorical":
      return value.value.label;
    case "text":
      return value.value;
  }
}

function usageLine(usage: Schema<"EvaluatorTestUsage">): string {
  const parts: string[] = [];
  if (usage.input_tokens !== undefined) {
    parts.push(`${usage.input_tokens} in`);
  }
  if (usage.output_tokens !== undefined) {
    parts.push(`${usage.output_tokens} out`);
  }
  if (usage.cost_usd !== undefined) {
    parts.push(`$${usage.cost_usd}`);
  }
  return parts.join(" · ");
}

/** The message of a failed run. The rate limit and a missing trace have their own wording. */
function failureMessage(error: unknown): string {
  const problem = problemOf(error);
  if (problem?.status === 429) {
    return "Too many tests. Wait a minute.";
  }
  if (problem?.status === 404) {
    return "Trace not found.";
  }
  const messages = Object.values(fieldErrors(error));
  if (messages.length > 0) {
    return messages.join(" ");
  }
  return problemMessage(error, "Could not run the test.");
}

/**
 * Runs the evaluator in the form once, on a stored trace with a variable mapping or on literal
 * variables. Nothing is saved and no score is written (D-I-21).
 */
export function TestPanel({
  org,
  project,
  evaluatorId,
  body,
  dirty,
  defaultMapping,
}: {
  org: string;
  project: string;
  evaluatorId?: string;
  /** The form's current evaluator, or `null` when the form has client errors. */
  body: Body | null;
  /** Whether the form differs from the saved evaluator. Always true on new. */
  dirty: boolean;
  defaultMapping: Mapping[];
}) {
  const [mode, setMode] = useState<"trace" | "variables">("trace");
  const [traceId, setTraceId] = useState("");
  const [spanId, setSpanId] = useState("");
  const [mapping, setMapping] = useState<MappingRow[]>(() =>
    initialMapping(defaultMapping, promptOf(body)),
  );
  const [variables, setVariables] = useState<VariableRow[]>(() => [newVariableRow()]);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const test = $api.useMutation("post", "/api/v1/projects/{project}/evaluators/test", {
    gcTime: 0,
  });

  // The result holds the rendered prompt and variables, which can be payload (D-L-5).
  const resetTest = test.reset;
  useEffect(
    () => () => {
      resetTest();
    },
    [resetTest],
  );

  function runFields(): RunFields {
    const problems: Record<string, string> = {};
    if (mode === "variables") {
      const values: Record<string, unknown> = {};
      const seen = new Set<string>();
      for (const row of variables) {
        const name = row.name.trim();
        if (name.length === 0 && row.value.trim().length === 0) {
          continue;
        }
        if (name.length === 0) {
          problems.variables ??= "Enter a name for each variable.";
          continue;
        }
        if (seen.has(name)) {
          problems.variables ??= "Variable names must be unique.";
          continue;
        }
        seen.add(name);
        values[name] = parseValue(row.value);
      }
      return { fields: { variables: values }, errors: problems };
    }

    const trace = traceId.trim();
    if (!TRACE_ID.test(trace)) {
      problems.traceId = "Enter a 32-digit hex trace ID.";
    }
    const span = spanId.trim();
    if (span.length > 0 && !SPAN_ID.test(span)) {
      problems.spanId = "Enter a 16-digit hex span ID.";
    }
    const expressions: Record<string, string> = {};
    for (const row of mapping) {
      const variable = row.variable.trim();
      const expression = row.expression.trim();
      if (variable.length === 0 && expression.length === 0) {
        continue;
      }
      if (variable.length === 0) {
        problems.mapping ??= "Enter a variable name for each row.";
      } else if (expression.length === 0) {
        problems.mapping ??= "Enter an expression for each variable.";
      } else {
        expressions[variable] = expression;
      }
    }
    return {
      fields: {
        trace_id: trace,
        variable_mapping: expressions,
        ...(span.length > 0 ? { span_id: span } : {}),
      },
      errors: problems,
    };
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (body === null) {
      return;
    }
    const run = runFields();
    if (Object.keys(run.errors).length > 0) {
      setErrors(run.errors);
      return;
    }
    setErrors({});
    // An unchanged saved evaluator runs by id alone, so the server uses its sealed settings.
    const request = {
      ...(evaluatorId !== undefined ? { evaluator_id: evaluatorId } : {}),
      ...(evaluatorId === undefined || dirty ? { evaluator: body } : {}),
      ...run.fields,
    };
    test.mutate({
      params: { path: { project: projectRef(org, project) } },
      body: request as Schema<"EvaluatorTestRequest">,
    });
  }

  const result = test.data;
  const sectionError = errors.mapping ?? errors.variables;
  const note =
    body === null
      ? "Fix the form first."
      : evaluatorId === undefined
        ? "Runs the evaluator as it is in the form. Nothing is saved."
        : dirty
          ? "Runs the unsaved changes. Nothing is saved."
          : "Runs the saved evaluator.";

  return (
    <section aria-labelledby="test-panel-title" className="flex flex-col gap-4 border-t pt-6">
      <h2 id="test-panel-title" className="text-base font-medium">
        Test
      </h2>
      <form noValidate className="flex flex-col gap-4" onSubmit={onSubmit}>
        <Tabs
          value={mode}
          onValueChange={(value) => {
            setMode(value === "variables" ? "variables" : "trace");
          }}
        >
          <TabsList>
            <TabsTrigger value="trace">Trace</TabsTrigger>
            <TabsTrigger value="variables">Variables</TabsTrigger>
          </TabsList>
          <TabsContent value="trace" className="flex flex-col gap-4">
            <TextField
              id="test-trace-id"
              label="Trace ID"
              value={traceId}
              error={errors.traceId}
              onChange={setTraceId}
            />
            <TextField
              id="test-span-id"
              label="Span ID"
              value={spanId}
              error={errors.spanId}
              description="Optional. Targets one span of the trace."
              onChange={setSpanId}
            />
            <div className="flex flex-col gap-2">
              <span className="text-sm leading-none font-medium">Variable mapping</span>
              {mapping.map((row, index) => (
                <div key={row.id} className="flex items-center gap-2">
                  <Input
                    aria-label={`Mapping variable ${index + 1}`}
                    placeholder="Variable"
                    value={row.variable}
                    onChange={(event) => {
                      const variable = event.target.value;
                      setMapping(
                        mapping.map((item) => (item.id === row.id ? { ...item, variable } : item)),
                      );
                    }}
                  />
                  <Input
                    aria-label={`Mapping expression ${index + 1}`}
                    placeholder="trace.input"
                    value={row.expression}
                    onChange={(event) => {
                      const expression = event.target.value;
                      setMapping(
                        mapping.map((item) =>
                          item.id === row.id ? { ...item, expression } : item,
                        ),
                      );
                    }}
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    aria-label={`Remove mapping ${index + 1}`}
                    onClick={() => {
                      setMapping(mapping.filter((item) => item.id !== row.id));
                    }}
                  >
                    <X />
                  </Button>
                </div>
              ))}
              <div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setMapping([...mapping, newMappingRow("", "")]);
                  }}
                >
                  Add variable
                </Button>
              </div>
            </div>
          </TabsContent>
          <TabsContent value="variables" className="flex flex-col gap-4">
            <div className="flex flex-col gap-2">
              <span className="text-sm leading-none font-medium">Variables</span>
              {variables.map((row, index) => (
                <div key={row.id} className="flex items-center gap-2">
                  <Input
                    aria-label={`Variable name ${index + 1}`}
                    placeholder="Name"
                    value={row.name}
                    onChange={(event) => {
                      const name = event.target.value;
                      setVariables(
                        variables.map((item) => (item.id === row.id ? { ...item, name } : item)),
                      );
                    }}
                  />
                  <Input
                    aria-label={`Variable value ${index + 1}`}
                    placeholder="Value (JSON or text)"
                    value={row.value}
                    onChange={(event) => {
                      const value = event.target.value;
                      setVariables(
                        variables.map((item) => (item.id === row.id ? { ...item, value } : item)),
                      );
                    }}
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    aria-label={`Remove variable ${index + 1}`}
                    onClick={() => {
                      setVariables(variables.filter((item) => item.id !== row.id));
                    }}
                  >
                    <X />
                  </Button>
                </div>
              ))}
              <div>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setVariables([...variables, newVariableRow()]);
                  }}
                >
                  Add variable
                </Button>
              </div>
            </div>
          </TabsContent>
        </Tabs>
        {sectionError ? (
          <p role="alert" className="text-xs text-destructive">
            {sectionError}
          </p>
        ) : null}
        <FormError message={test.error ? failureMessage(test.error) : undefined} />
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="text-xs text-muted-foreground">{note}</p>
          <SubmitButton pending={test.isPending} disabled={body === null}>
            {test.isPending ? "Running…" : "Run test"}
          </SubmitButton>
        </div>
      </form>
      {result ? <TestResult org={org} project={project} result={result} /> : null}
    </section>
  );
}

function TestResult({
  org,
  project,
  result,
}: {
  org: string;
  project: string;
  result: Schema<"EvaluatorTestResult">;
}) {
  const usage = result.usage ? usageLine(result.usage) : "";
  const variables = Object.entries(result.variables);
  return (
    <section aria-label="Test result" className="flex flex-col gap-3 rounded-lg border p-4 text-sm">
      {result.ok && result.value ? (
        <p className="text-lg font-medium">{formatValue(result.value)}</p>
      ) : null}
      {result.ok && result.reason ? <p>{result.reason}</p> : null}
      {!result.ok ? (
        <div className="flex flex-col gap-1">
          <p className="font-medium text-destructive">Test failed</p>
          <p>{result.error?.message}</p>
          {result.error ? (
            <p className="text-xs text-muted-foreground">{HINTS[result.error.class]}</p>
          ) : null}
        </div>
      ) : null}
      <p className="text-xs text-muted-foreground">
        {[usage, `${result.latency_ms} ms`, result.trace_truncated ? "Trace truncated" : ""]
          .filter((part) => part.length > 0)
          .join(" · ")}
      </p>
      {result.prompt !== undefined ? (
        <details>
          <summary className="cursor-pointer font-medium">Prompt</summary>
          <pre className="mt-2 font-mono text-xs break-words whitespace-pre-wrap">
            {result.prompt}
          </pre>
        </details>
      ) : null}
      {variables.length > 0 ? (
        <details>
          <summary className="cursor-pointer font-medium">Variables</summary>
          <dl className="mt-2 flex flex-col gap-2">
            {variables.map(([name, value]) => (
              <div key={name} className="flex flex-col gap-0.5">
                <dt className="font-mono text-xs">{name}</dt>
                <dd className="font-mono text-xs break-words whitespace-pre-wrap">{value}</dd>
              </div>
            ))}
          </dl>
        </details>
      ) : null}
      {result.judge_trace_id ? (
        <Link
          to="/$org/$project/traces/$traceId"
          params={{ org, project, traceId: result.judge_trace_id }}
          className="w-fit text-xs underline underline-offset-4"
        >
          View judge trace
        </Link>
      ) : null}
    </section>
  );
}
