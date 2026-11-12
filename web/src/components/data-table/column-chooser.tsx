import { ChevronDown, ChevronUp } from "lucide-react";
import { reorderVisible } from "@/components/data-table/column-layout";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";

export function ColumnChooser({
  columns,
  visible,
  onVisible,
}: {
  columns: readonly { id: string; label: string; locked?: boolean; defaultVisible?: boolean }[];
  visible: readonly string[];
  onVisible: (ids: string[]) => void;
}) {
  const byId = new Map(columns.map((column) => [column.id, column]));
  const shown = visible.flatMap((id) => {
    const column = byId.get(id);
    return column ? [column] : [];
  });
  const hidden = columns.filter((column) => !visible.includes(column.id));

  function toggle(id: string, on: boolean) {
    if (on) {
      onVisible([...visible, id]);
      return;
    }
    onVisible(visible.filter((item) => item !== id));
  }

  function move(id: string, delta: -1 | 1) {
    const next = reorderVisible(visible, id, delta);
    if (next) {
      onVisible(next);
    }
  }

  function reset() {
    onVisible(columns.filter((column) => column.defaultVisible).map((column) => column.id));
  }

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button type="button" variant="outline" size="xs">
          Columns
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-56 gap-2 p-2">
        <ul className="flex max-h-80 flex-col gap-1 overflow-auto">
          {shown.map((column, index) => (
            <ColumnRow
              key={column.id}
              label={column.label}
              checked
              locked={column.locked}
              upDisabled={index === 0}
              downDisabled={index === shown.length - 1}
              onCheckedChange={(on) => toggle(column.id, on)}
              onUp={() => move(column.id, -1)}
              onDown={() => move(column.id, 1)}
            />
          ))}
          {hidden.map((column) => (
            <ColumnRow
              key={column.id}
              label={column.label}
              checked={false}
              onCheckedChange={(on) => toggle(column.id, on)}
            />
          ))}
        </ul>
        <Button type="button" variant="ghost" size="xs" className="self-start" onClick={reset}>
          Reset
        </Button>
      </PopoverContent>
    </Popover>
  );
}

function ColumnRow({
  label,
  checked,
  locked,
  upDisabled,
  downDisabled,
  onCheckedChange,
  onUp,
  onDown,
}: {
  label: string;
  checked: boolean;
  locked?: boolean;
  upDisabled?: boolean;
  downDisabled?: boolean;
  onCheckedChange: (on: boolean) => void;
  onUp?: () => void;
  onDown?: () => void;
}) {
  return (
    <li className="flex items-center gap-1">
      <label className="flex min-w-0 flex-1 items-center gap-2 text-xs">
        <input
          type="checkbox"
          className="size-3.5 accent-primary"
          checked={checked}
          disabled={locked}
          onChange={(event) => onCheckedChange(event.target.checked)}
        />
        <span className="truncate">{label}</span>
      </label>
      {onUp && onDown ? (
        <span className="flex shrink-0">
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label={`Move ${label} up`}
            disabled={upDisabled}
            onClick={onUp}
          >
            <ChevronUp />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label={`Move ${label} down`}
            disabled={downDisabled}
            onClick={onDown}
          >
            <ChevronDown />
          </Button>
        </span>
      ) : null}
    </li>
  );
}
