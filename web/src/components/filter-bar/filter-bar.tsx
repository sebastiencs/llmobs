import { useQuery } from "@tanstack/react-query";
import { cn } from "cn";
import { X } from "lucide-react";
import { type FormEvent, type ReactNode, useState } from "react";
import type { Problem } from "@/api/problem";
import { schemaQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { useShortcut } from "@/components/command/use-shortcut";
import { FilterEditor } from "@/components/filter-bar/editor";
import {
  chipFromDraft,
  chipOperators,
  type Draft,
  draftFromChip,
  filterErrorRange,
  filterMessage,
  keepsName,
  needsValue,
  targetKind,
} from "@/components/filter-bar/logic";
import { useProject } from "@/components/shell/project-context";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { type Chip, parseChip, serializeFilter, splitConjuncts } from "@/lib/dsl/dsl";

type ViewName = Schema<"ViewName">;

export function FilterBar({
  view,
  value,
  onApply,
  error,
  placeholder,
}: {
  view: ViewName;
  value: string;
  onApply: (text: string) => void;
  error?: Problem;
  placeholder?: string;
}) {
  const { org, project } = useProject();
  const schema = useQuery(schemaQuery(org, project));
  const [textMode, setTextMode] = useState<{ from?: number; to?: number } | null>(null);
  const columns = schema.data?.views.find((item) => item.name === view)?.columns ?? [];
  const showText = textMode !== null || error !== undefined;
  const errorRange = error ? filterErrorRange(value, error) : null;
  const selection =
    textMode?.from !== undefined && textMode.to !== undefined
      ? { from: textMode.from, to: textMode.to }
      : undefined;

  useShortcut("filter-text", (event) => {
    if (showText) {
      return;
    }
    event.preventDefault();
    setTextMode({});
  });

  function apply(text: string) {
    setTextMode(null);
    onApply(text);
  }

  const conjuncts = splitConjuncts(value).map((part) => ({ ...part, chip: parseChip(part.text) }));

  return (
    <div className="flex min-w-0 flex-col">
      <div
        className={cn(
          "flex h-7 min-w-0 items-center gap-1 rounded-md border bg-background px-1",
          error ? "border-destructive" : "border-input",
        )}
      >
        {showText ? (
          <FilterEditor
            value={value}
            selection={selection}
            errorRange={errorRange}
            viewName={view}
            schema={schema.data}
            placeholder={placeholder}
            onApply={apply}
            onCancel={() => {
              if (!error) {
                setTextMode(null);
              }
            }}
          />
        ) : (
          <Chips
            conjuncts={conjuncts}
            columns={columns}
            view={view}
            onApply={apply}
            placeholder={placeholder}
            onEditText={(range) => setTextMode(range ?? {})}
          />
        )}
      </div>
      {error ? (
        <p className="text-[12px] leading-4 break-words text-destructive">{filterMessage(error)}</p>
      ) : null}
    </div>
  );
}

function Chips({
  conjuncts,
  columns,
  view,
  onApply,
  placeholder = "Filter…",
  onEditText,
}: {
  conjuncts: { text: string; start: number; end: number; chip: Chip }[];
  columns: Schema<"ColumnSchema">[];
  view: ViewName;
  onApply: (text: string) => void;
  placeholder?: string;
  onEditText: (range?: { from: number; to: number }) => void;
}) {
  function replace(index: number, chip: Chip) {
    onApply(serializeFilter(conjuncts.map((part, i) => (i === index ? chip : part.chip))));
  }
  function remove(index: number) {
    onApply(serializeFilter(conjuncts.filter((_, i) => i !== index).map((part) => part.chip)));
  }
  return (
    <div className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
      {conjuncts.map((part, index) =>
        part.chip.kind === "raw" ? (
          <ChipShell
            key={`${part.start}:${part.text}`}
            text={part.text}
            onRemove={() => remove(index)}
          >
            <button
              type="button"
              className="truncate px-1.5"
              onClick={() => onEditText({ from: part.start, to: part.end })}
            >
              {part.text}
            </button>
          </ChipShell>
        ) : (
          <ChipEditor
            key={`${part.start}:${part.text}`}
            text={part.text}
            chip={part.chip}
            columns={columns}
            view={view}
            onRemove={() => remove(index)}
            onApply={(chip) => replace(index, chip)}
          />
        ),
      )}
      <button
        type="button"
        aria-label="Edit filter"
        className="h-6 min-w-8 flex-1 truncate px-1 text-left text-xs text-muted-foreground"
        onClick={() => onEditText()}
      >
        {conjuncts.length === 0 ? placeholder : ""}
      </button>
      <button
        type="button"
        className="shrink-0 px-1 text-xs text-muted-foreground"
        onClick={() => onEditText()}
      >
        Edit as text
      </button>
    </div>
  );
}

function ChipShell({
  text,
  onRemove,
  children,
}: {
  text: string;
  onRemove: () => void;
  children: ReactNode;
}) {
  return (
    <span className="inline-flex h-6 max-w-64 shrink-0 items-center rounded-md bg-muted text-xs">
      {children}
      <button
        type="button"
        aria-label={`Remove ${text}`}
        className="px-1 text-muted-foreground"
        onClick={(event) => {
          event.stopPropagation();
          onRemove();
        }}
      >
        <X className="size-3" />
      </button>
    </span>
  );
}

function ChipEditor({
  text,
  chip,
  columns,
  view,
  onRemove,
  onApply,
}: {
  text: string;
  chip: Chip;
  columns: Schema<"ColumnSchema">[];
  view: ViewName;
  onRemove: () => void;
  onApply: (chip: Chip) => void;
}) {
  const initial = draftFromChip(chip);
  const [open, setOpen] = useState(false);
  if (!initial) {
    return null;
  }
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <ChipShell text={text} onRemove={onRemove}>
        <PopoverTrigger asChild>
          <button type="button" className="truncate px-1.5">
            {text}
          </button>
        </PopoverTrigger>
      </ChipShell>
      <PopoverContent align="start" className="w-72 gap-2 p-3">
        <ChipForm
          initial={initial}
          columns={columns}
          view={view}
          onApply={(next) => {
            onApply(next);
            setOpen(false);
          }}
        />
      </PopoverContent>
    </Popover>
  );
}

function ChipForm({
  initial,
  columns,
  view,
  onApply,
}: {
  initial: Draft;
  columns: Schema<"ColumnSchema">[];
  view: ViewName;
  onApply: (chip: Chip) => void;
}) {
  const [draft, setDraft] = useState(initial);
  const kind = targetKind(draft.target, columns);
  const operators = chipOperators(kind);
  const op = operators.includes(draft.op) ? draft.op : (operators[0] ?? draft.op);
  const next = chipFromDraft({ ...draft, op }, kind);
  const names = columns.map((column) => column.name);
  if (!names.includes(draft.target) && !isCall(draft.target)) {
    names.unshift(draft.target);
  }
  const calls = view === "traces" || view === "spans" || view === "sessions";
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(event: FormEvent) => {
        event.preventDefault();
        if (next) {
          onApply(next);
        }
      }}
    >
      <label className="flex flex-col gap-1 text-xs">
        Field
        <select
          aria-label="Field"
          className="h-7 rounded-md border border-input bg-transparent px-1"
          value={draft.target}
          onChange={(event) => {
            const target = event.target.value;
            setDraft((current) => ({
              ...current,
              target,
              name: keepsName(current.target, target) ? current.name : "",
            }));
          }}
        >
          {names.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
          {calls ? (
            <>
              <option value="score">score</option>
              <option value="score_label">score_label</option>
              <option value="has_score">has_score</option>
            </>
          ) : null}
        </select>
      </label>
      {kind === "string_map" || isCall(draft.target) ? (
        <label className="flex flex-col gap-1 text-xs">
          {kind === "string_map" ? "Key" : "Score"}
          <input
            aria-label={kind === "string_map" ? "Key" : "Score"}
            className="h-7 rounded-md border border-input bg-transparent px-2"
            value={draft.name}
            onChange={(event) => {
              const name = event.target.value;
              setDraft((current) => ({ ...current, name }));
            }}
          />
        </label>
      ) : null}
      <label className="flex flex-col gap-1 text-xs">
        Operator
        <select
          aria-label="Operator"
          className="h-7 rounded-md border border-input bg-transparent px-1"
          value={op}
          onChange={(event) => {
            const chosen = event.target.value;
            setDraft((current) => ({ ...current, op: chosen }));
          }}
        >
          {operators.map((item) => (
            <option key={item} value={item}>
              {item}
            </option>
          ))}
        </select>
      </label>
      {needsValue(op) ? (
        <label className="flex flex-col gap-1 text-xs">
          Value
          <input
            aria-label="Value"
            className="h-7 rounded-md border border-input bg-transparent px-2 font-mono"
            value={draft.value}
            onChange={(event) => {
              const input = event.target.value;
              setDraft((current) => ({ ...current, value: input }));
            }}
          />
        </label>
      ) : null}
      <Button type="submit" size="xs" disabled={next === null}>
        Apply
      </Button>
    </form>
  );
}

function isCall(target: string): boolean {
  return target === "score" || target === "score_label" || target === "has_score";
}
