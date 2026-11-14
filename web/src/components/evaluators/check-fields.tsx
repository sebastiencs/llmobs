import { Plus, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";

/** The six built-in checks, in the order of `crates/eval/src/config.rs` `Check`. */
export const CHECK_NAMES = [
  "exact_match",
  "contains",
  "regex",
  "json_valid",
  "json_schema",
  "levenshtein",
] as const;

export type CheckName = (typeof CHECK_NAMES)[number];

export const CHECK_LABELS: Record<CheckName, string> = {
  exact_match: "Exact match",
  contains: "Contains",
  regex: "Regex",
  json_valid: "Valid JSON",
  json_schema: "JSON Schema",
  levenshtein: "Levenshtein similarity",
};

export interface NeedleRow {
  id: number;
  text: string;
}

/** Every parameter any check edits. A check reads only its own fields. */
export interface CheckParams {
  caseSensitive: boolean;
  trim: boolean;
  needles: NeedleRow[];
  mode: "any" | "all";
  pattern: string;
  schema: string;
  normalize: boolean;
}

let needleRowId = 0;

function needleRow(text: string): NeedleRow {
  needleRowId += 1;
  return { id: needleRowId, text };
}

/** The defaults the form shows when a check is picked. */
export function defaultCheckParams(check: CheckName): CheckParams {
  return {
    caseSensitive: check === "exact_match",
    trim: true,
    needles: [needleRow("")],
    mode: "any",
    pattern: "",
    schema: "{}",
    normalize: true,
  };
}

/** The form state of a saved check config (`Check` JSON as the API stores it). */
export function checkStateOf(config: Record<string, unknown>): {
  check: CheckName;
  params: CheckParams;
} {
  const check = CHECK_NAMES.find((name) => name === config.check) ?? "exact_match";
  const defaults = defaultCheckParams(check);
  const values = Array.isArray(config.values)
    ? config.values.filter((value): value is string => typeof value === "string")
    : [];
  return {
    check,
    params: {
      ...defaults,
      caseSensitive:
        typeof config.case_sensitive === "boolean" ? config.case_sensitive : defaults.caseSensitive,
      trim: typeof config.trim === "boolean" ? config.trim : defaults.trim,
      needles: values.length > 0 ? values.map((value) => needleRow(value)) : defaults.needles,
      mode: config.mode === "all" ? "all" : "any",
      pattern: typeof config.pattern === "string" ? config.pattern : "",
      schema:
        config.schema === undefined ? defaults.schema : JSON.stringify(config.schema, null, 2),
      normalize: typeof config.normalize === "boolean" ? config.normalize : defaults.normalize,
    },
  };
}

/** The output data type a check writes: numeric for `levenshtein`, boolean otherwise. */
export function checkDataType(check: CheckName): "numeric" | "boolean" {
  return check === "levenshtein" ? "numeric" : "boolean";
}

export interface CheckBuild {
  /** The `config` body: `{check, ...fields}` with only the fields the check reads. */
  config: Record<string, unknown>;
  /** Client errors, keyed by the control they belong to. Empty when the config is valid. */
  errors: Record<string, string>;
}

/** Turns the form state of a check into its `config`, and reports the client errors that block a submit. */
export function buildCheckConfig(check: CheckName, params: CheckParams): CheckBuild {
  switch (check) {
    case "exact_match":
      return {
        config: { check, case_sensitive: params.caseSensitive, trim: params.trim },
        errors: {},
      };
    case "contains": {
      const values = params.needles.map((row) => row.text).filter((text) => text.trim().length > 0);
      // Never sends `value`: `values` alone is the stored form. Without needles the check compares with `expected`.
      const config: Record<string, unknown> = {
        check,
        mode: params.mode,
        case_sensitive: params.caseSensitive,
      };
      if (values.length > 0) {
        config.values = values;
      }
      return { config, errors: {} };
    }
    case "regex":
      if (params.pattern.trim().length === 0) {
        return { config: { check }, errors: { pattern: "Enter a pattern." } };
      }
      return { config: { check, pattern: params.pattern }, errors: {} };
    case "json_valid":
      return { config: { check }, errors: {} };
    case "json_schema": {
      const parsed = parseSchema(params.schema);
      if (parsed === undefined) {
        return { config: { check }, errors: { schema: "The schema must be a JSON object." } };
      }
      return { config: { check, schema: parsed }, errors: {} };
    }
    case "levenshtein":
      return { config: { check, normalize: params.normalize }, errors: {} };
  }
}

function parseSchema(text: string): Record<string, unknown> | undefined {
  try {
    const parsed: unknown = JSON.parse(text);
    return typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : undefined;
  } catch {
    return undefined;
  }
}

/** The form for one check's parameters. Validation lives in `buildCheckConfig`. */
export function CheckFields({
  check,
  value,
  onChange,
  errors,
}: {
  check: CheckName;
  value: CheckParams;
  onChange: (next: CheckParams) => void;
  errors: Record<string, string>;
}) {
  function patch(change: Partial<CheckParams>) {
    onChange({ ...value, ...change });
  }

  switch (check) {
    case "exact_match":
      return (
        <div className="flex flex-col gap-3">
          <SwitchField
            id="check-case-sensitive"
            label="Case sensitive"
            checked={value.caseSensitive}
            onChange={(caseSensitive) => {
              patch({ caseSensitive });
            }}
          />
          <SwitchField
            id="check-trim"
            label="Trim whitespace"
            checked={value.trim}
            onChange={(trim) => {
              patch({ trim });
            }}
          />
        </div>
      );
    case "contains":
      return (
        <div className="flex flex-col gap-3">
          <div className="flex flex-col gap-2">
            <span className="text-sm leading-none font-medium">Text to find</span>
            {value.needles.map((row, index) => (
              <div key={row.id} className="flex items-center gap-2">
                <Input
                  aria-label={`Text to find ${index + 1}`}
                  aria-invalid={errors.values ? true : undefined}
                  value={row.text}
                  onChange={(event) => {
                    patch({
                      needles: value.needles.map((item) =>
                        item.id === row.id ? { ...item, text: event.target.value } : item,
                      ),
                    });
                  }}
                />
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  aria-label={`Remove text ${index + 1}`}
                  onClick={() => {
                    patch({ needles: value.needles.filter((item) => item.id !== row.id) });
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
                patch({ needles: [...value.needles, needleRow("")] });
              }}
            >
              <Plus />
              Add text
            </Button>
            {value.needles.every((row) => row.text.trim().length === 0) ? (
              <p className="text-xs text-muted-foreground">
                No text: the output is compared with the expected value.
              </p>
            ) : null}
            {errors.values ? (
              <p role="alert" className="text-xs text-destructive">
                {errors.values}
              </p>
            ) : null}
          </div>
          <RadioGroup
            aria-label="Match"
            value={value.mode}
            onValueChange={(mode) => {
              patch({ mode: mode === "all" ? "all" : "any" });
            }}
            className="flex-row gap-4"
          >
            <div className="flex items-center gap-2">
              <RadioGroupItem value="any" id="check-mode-any" />
              <Label htmlFor="check-mode-any">Any</Label>
            </div>
            <div className="flex items-center gap-2">
              <RadioGroupItem value="all" id="check-mode-all" />
              <Label htmlFor="check-mode-all">All</Label>
            </div>
          </RadioGroup>
          <SwitchField
            id="check-case-sensitive"
            label="Case sensitive"
            checked={value.caseSensitive}
            onChange={(caseSensitive) => {
              patch({ caseSensitive });
            }}
          />
        </div>
      );
    case "regex":
      return (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="check-pattern">Pattern</Label>
          <Input
            id="check-pattern"
            className="font-mono"
            value={value.pattern}
            aria-invalid={errors.pattern ? true : undefined}
            aria-describedby="check-pattern-description"
            onChange={(event) => {
              patch({ pattern: event.target.value });
            }}
          />
          <p id="check-pattern-description" className="text-xs text-muted-foreground">
            Rust regex syntax, searched anywhere in the output.
          </p>
          {errors.pattern ? (
            <p role="alert" className="text-xs text-destructive">
              {errors.pattern}
            </p>
          ) : null}
        </div>
      );
    case "json_valid":
      return (
        <p className="text-sm text-muted-foreground">
          Passes when the whole output parses as JSON.
        </p>
      );
    case "json_schema":
      return (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor="check-schema">JSON Schema</Label>
          <Textarea
            id="check-schema"
            className="font-mono"
            rows={12}
            value={value.schema}
            aria-invalid={errors.schema ? true : undefined}
            onChange={(event) => {
              patch({ schema: event.target.value });
            }}
          />
          {errors.schema ? (
            <p role="alert" className="text-xs text-destructive">
              {errors.schema}
            </p>
          ) : null}
        </div>
      );
    case "levenshtein":
      return (
        <SwitchField
          id="check-normalize"
          label="Normalize (trim, ignore case, collapse spaces)"
          checked={value.normalize}
          onChange={(normalize) => {
            patch({ normalize });
          }}
        />
      );
  }
}

function SwitchField({
  id,
  label,
  checked,
  onChange,
}: {
  id: string;
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <Switch id={id} size="sm" checked={checked} onCheckedChange={onChange} />
      <Label htmlFor={id}>{label}</Label>
    </div>
  );
}
