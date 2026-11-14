import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { SqlParam } from "@/lib/sql/params";

const PLACEHOLDERS: Partial<Record<SqlParam["type"], string>> = {
  Int: "42",
  Float: "0.5",
  Timestamp: "2026-01-01T00:00:00Z",
};

export function ParamsBar({
  params,
  values,
  errors,
  onChange,
}: {
  params: readonly SqlParam[];
  values: Readonly<Record<string, string>>;
  /** Set by a run attempt; the console clears a name's error when it changes. */
  errors: Readonly<Record<string, string>>;
  onChange: (name: string, value: string) => void;
}) {
  if (params.length === 0) {
    return null;
  }
  return (
    <fieldset className="flex shrink-0 flex-wrap items-start gap-3">
      <legend className="sr-only">Parameters</legend>
      {params.map((param) => {
        const id = `sql-param-${param.name}`;
        const error = errors[param.name];
        const value = values[param.name] ?? "";
        const described = error ? { "aria-invalid": true, "aria-describedby": `${id}-error` } : {};
        return (
          <div key={param.name} className="flex flex-col gap-1">
            <Label htmlFor={id} className="font-mono text-xs">
              {`${param.name} (${param.type})`}
            </Label>
            {param.type === "Bool" ? (
              <select
                id={id}
                {...described}
                className="focus-ring h-7 rounded-md border border-border bg-background px-1.5 text-xs"
                value={value}
                onChange={(event) => onChange(param.name, event.target.value)}
              >
                <option value="">—</option>
                <option value="true">true</option>
                <option value="false">false</option>
              </select>
            ) : (
              <Input
                id={id}
                {...described}
                value={value}
                className="h-7 w-48 font-mono text-xs"
                placeholder={PLACEHOLDERS[param.type] ?? ""}
                onChange={(event) => onChange(param.name, event.target.value)}
              />
            )}
            {error ? (
              <p id={`${id}-error`} className="text-destructive text-xs">
                {error}
              </p>
            ) : null}
          </div>
        );
      })}
    </fieldset>
  );
}
