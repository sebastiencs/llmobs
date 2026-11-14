import { Plus, X } from "lucide-react";
import type { Schema } from "@/api/types";
import { TextField } from "@/components/form/text-field";
import { Button } from "@/components/ui/button";
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

export type OutputKind = "numeric" | "boolean" | "categorical";

export interface LabelRow {
  id: number;
  label: string;
  value: string;
}

/** The form state of an output: a judge's `output`, or the data type of a webhook's score. */
export interface OutputState {
  type: OutputKind;
  min: string;
  max: string;
  labels: LabelRow[];
}

let labelRowId = 0;

function labelRow(label: string, value: string): LabelRow {
  labelRowId += 1;
  return { id: labelRowId, label, value };
}

const KIND_LABELS: Record<OutputKind, string> = {
  numeric: "Numeric",
  boolean: "Boolean",
  categorical: "Categorical",
};

export function defaultOutputState(): OutputState {
  return { type: "numeric", min: "0", max: "1", labels: [labelRow("", "")] };
}

/** The form state of a judge `output` (`JudgeOutput` JSON). */
export function outputStateOfJudge(output: unknown): OutputState {
  const record = isRecord(output) ? output : {};
  const defaults = defaultOutputState();
  if (record.type === "boolean") {
    return { ...defaults, type: "boolean" };
  }
  if (record.type === "categorical") {
    const labels = Array.isArray(record.labels)
      ? record.labels.filter((label): label is string => typeof label === "string")
      : [];
    const values = isRecord(record.label_values) ? record.label_values : {};
    return {
      ...defaults,
      type: "categorical",
      labels: labels.map((label) => {
        const value = values[label];
        return labelRow(label, typeof value === "number" ? String(value) : "");
      }),
    };
  }
  return {
    ...defaults,
    type: "numeric",
    min: typeof record.min === "number" ? String(record.min) : defaults.min,
    max: typeof record.max === "number" ? String(record.max) : defaults.max,
  };
}

/** The form state of a stored score's data type, for a webhook. Its categories are its labels. */
export function outputStateOfScore(score: Schema<"OutputScoreBody">): OutputState {
  const defaults = defaultOutputState();
  if (score.data_type === "boolean") {
    return { ...defaults, type: "boolean" };
  }
  if (score.data_type === "categorical") {
    return {
      ...defaults,
      type: "categorical",
      labels: (score.categories ?? []).map((label) => labelRow(label, "")),
    };
  }
  return { ...defaults, type: "numeric" };
}

export interface OutputBuild {
  /** The judge's `output` JSON. Only built for a judge. */
  output?: Record<string, unknown>;
  /** The data type and categories of the score the output writes. */
  score: { data_type: OutputKind; categories?: string[] };
  /** Client errors, keyed `min`, `max` or `labels`. Empty when the output is valid. */
  errors: Record<string, string>;
}

/** Builds the output JSON of a judge (`judge` true) or the score data type of a webhook. */
export function buildOutput(state: OutputState, judge: boolean): OutputBuild {
  const errors: Record<string, string> = {};
  if (state.type === "boolean") {
    return {
      ...(judge ? { output: { type: "boolean" } } : {}),
      score: { data_type: "boolean" },
      errors,
    };
  }
  if (state.type === "categorical") {
    const rows = state.labels.filter((row) => row.label.trim().length > 0);
    const labels = rows.map((row) => row.label.trim());
    const minimum = judge ? 2 : 1;
    if (labels.length < minimum) {
      errors.labels = judge ? "Enter at least two labels." : "Enter at least one label.";
    }
    const labelValues: Record<string, number> = {};
    for (const row of rows) {
      if (row.value.trim().length === 0) {
        continue;
      }
      const number = Number(row.value);
      if (!Number.isFinite(number)) {
        errors.labels ??= "Label values must be numbers.";
      } else {
        labelValues[row.label.trim()] = number;
      }
    }
    return {
      ...(judge ? { output: { type: "categorical", labels, label_values: labelValues } } : {}),
      score: { data_type: "categorical", categories: labels },
      errors,
    };
  }
  if (!judge) {
    return { score: { data_type: "numeric" }, errors };
  }
  const min = Number(state.min);
  const max = Number(state.max);
  if (state.min.trim() === "" || !Number.isFinite(min)) {
    errors.min = "Enter a number.";
  }
  if (state.max.trim() === "" || !Number.isFinite(max)) {
    errors.max = "Enter a number.";
  }
  if (errors.min === undefined && errors.max === undefined && min >= max) {
    errors.max = "Maximum must be above minimum.";
  }
  return {
    output: { type: "numeric", min, max },
    score: { data_type: "numeric" },
    errors,
  };
}

/** The form for an output. `judge` shows the judge's numeric range and label values. */
export function OutputFields({
  judge,
  value,
  onChange,
  errors,
}: {
  judge: boolean;
  value: OutputState;
  onChange: (next: OutputState) => void;
  errors: Record<string, string>;
}) {
  function patch(change: Partial<OutputState>) {
    onChange({ ...value, ...change });
  }

  return (
    <div className="flex flex-col gap-4">
      {judge ? (
        <RadioGroup
          aria-label="Output type"
          value={value.type}
          onValueChange={(type) => {
            patch({ type: outputKindOf(type) });
          }}
          className="flex-row gap-4"
        >
          {(["numeric", "boolean", "categorical"] as const).map((kind) => (
            <div key={kind} className="flex items-center gap-2">
              <RadioGroupItem value={kind} id={`output-type-${kind}`} />
              <Label htmlFor={`output-type-${kind}`}>{KIND_LABELS[kind]}</Label>
            </div>
          ))}
        </RadioGroup>
      ) : (
        <div className="flex flex-col gap-2">
          <Label htmlFor="output-type">Output type</Label>
          <Select
            value={value.type}
            onValueChange={(type) => {
              patch({ type: outputKindOf(type) });
            }}
          >
            <SelectTrigger id="output-type" className="w-fit">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {(["numeric", "boolean", "categorical"] as const).map((kind) => (
                <SelectItem key={kind} value={kind}>
                  {KIND_LABELS[kind]}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}
      {judge && value.type === "numeric" ? (
        <div className="flex flex-col gap-4 sm:flex-row">
          <TextField
            id="output-min"
            type="number"
            label="Minimum"
            value={value.min}
            error={errors.min}
            onChange={(min) => {
              patch({ min });
            }}
          />
          <TextField
            id="output-max"
            type="number"
            label="Maximum"
            value={value.max}
            error={errors.max}
            onChange={(max) => {
              patch({ max });
            }}
          />
        </div>
      ) : null}
      {value.type === "categorical" ? (
        <div className="flex flex-col gap-2">
          <span className="text-sm leading-none font-medium">Labels</span>
          {value.labels.map((row, index) => (
            <div key={row.id} className="flex items-center gap-2">
              <Input
                aria-label={`Label ${index + 1}`}
                aria-invalid={errors.labels ? true : undefined}
                value={row.label}
                onChange={(event) => {
                  patch({
                    labels: value.labels.map((item) =>
                      item.id === row.id ? { ...item, label: event.target.value } : item,
                    ),
                  });
                }}
              />
              {judge ? (
                <Input
                  aria-label={`Value of label ${index + 1}`}
                  className="w-28"
                  placeholder="Value"
                  inputMode="decimal"
                  value={row.value}
                  onChange={(event) => {
                    patch({
                      labels: value.labels.map((item) =>
                        item.id === row.id ? { ...item, value: event.target.value } : item,
                      ),
                    });
                  }}
                />
              ) : null}
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={`Remove label ${index + 1}`}
                onClick={() => {
                  patch({ labels: value.labels.filter((item) => item.id !== row.id) });
                }}
              >
                <X />
              </Button>
            </div>
          ))}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="w-fit"
            onClick={() => {
              patch({ labels: [...value.labels, labelRow("", "")] });
            }}
          >
            <Plus />
            Add label
          </Button>
          {errors.labels ? (
            <p role="alert" className="text-xs text-destructive">
              {errors.labels}
            </p>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

function outputKindOf(value: string): OutputKind {
  return value === "boolean" || value === "categorical" ? value : "numeric";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
