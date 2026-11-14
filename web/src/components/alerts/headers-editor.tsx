import { Plus, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export type HeaderRow = { id: number; name: string; value: string };

const MAX_HEADERS = 10;

export function HeadersEditor({
  rows,
  onChange,
  nextId,
  description,
  error,
  disabled,
}: {
  rows: HeaderRow[];
  onChange: (rows: HeaderRow[]) => void;
  /** Creates the id of a new row. */
  nextId: () => number;
  /** Shown under the legend, as `<p id="channel-headers-description">`, and referenced by the fieldset's `aria-describedby`. */
  description?: string;
  error?: string;
  disabled?: boolean;
}) {
  const describedBy = [
    description ? "channel-headers-description" : null,
    error ? "channel-headers-error" : null,
  ]
    .filter((item): item is string => item !== null)
    .join(" ");

  return (
    <fieldset
      className="flex flex-col gap-2"
      disabled={disabled}
      aria-describedby={describedBy.length > 0 ? describedBy : undefined}
    >
      <legend className="text-sm font-medium">Headers</legend>
      {description ? (
        <p id="channel-headers-description" className="text-xs text-muted-foreground">
          {description}
        </p>
      ) : null}
      {rows.map((row, index) => (
        <div key={row.id} className="flex items-center gap-2">
          <Input
            aria-label={`Header name ${index + 1}`}
            value={row.name}
            aria-invalid={error ? true : undefined}
            autoComplete="off"
            onChange={(event) => {
              onChange(
                rows.map((item) =>
                  item.id === row.id ? { ...item, name: event.target.value } : item,
                ),
              );
            }}
          />
          <Input
            type="password"
            aria-label={`Header value ${index + 1}`}
            value={row.value}
            aria-invalid={error ? true : undefined}
            autoComplete="new-password"
            onChange={(event) => {
              onChange(
                rows.map((item) =>
                  item.id === row.id ? { ...item, value: event.target.value } : item,
                ),
              );
            }}
          />
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label={`Remove header ${index + 1}`}
            onClick={() => {
              onChange(rows.filter((item) => item.id !== row.id));
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
        disabled={rows.length >= MAX_HEADERS}
        onClick={() => {
          onChange([...rows, { id: nextId(), name: "", value: "" }]);
        }}
      >
        <Plus />
        Add header
      </Button>
      {error ? (
        <p id="channel-headers-error" className="text-xs text-destructive">
          {error}
        </p>
      ) : null}
    </fieldset>
  );
}
