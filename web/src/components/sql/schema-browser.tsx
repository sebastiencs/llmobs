import { ChevronRight, Plus } from "lucide-react";
import { useState } from "react";
import type { Schema } from "@/api/types";
import { Button } from "@/components/ui/button";

function toggled(set: ReadonlySet<string>, key: string): ReadonlySet<string> {
  const next = new Set(set);
  if (next.has(key)) {
    next.delete(key);
  } else {
    next.add(key);
  }
  return next;
}

export function SchemaBrowser({
  schema,
  failed,
  onInsert,
}: {
  schema: Schema<"SchemaResponse"> | undefined;
  /** The schema request failed. */
  failed: boolean;
  onInsert: (text: string) => void;
}) {
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set(["traces"]));
  if (failed) {
    return (
      <nav
        aria-label="Schema"
        className="flex w-56 shrink-0 flex-col gap-2 overflow-auto border-r p-2 text-xs"
      >
        <p className="text-destructive">Schema unavailable</p>
      </nav>
    );
  }
  if (schema === undefined) {
    return (
      <nav
        aria-label="Schema"
        className="flex w-56 shrink-0 flex-col gap-2 overflow-auto border-r p-2 text-xs"
      >
        <p className="text-muted-foreground">Loading…</p>
      </nav>
    );
  }
  return (
    <nav
      aria-label="Schema"
      className="flex w-56 shrink-0 flex-col gap-2 overflow-auto border-r p-2 text-xs"
    >
      {schema.views.map((view) => {
        const open = expanded.has(view.name);
        return (
          <div key={view.name}>
            <div className="flex items-center gap-1">
              <button
                type="button"
                aria-expanded={open}
                aria-controls={`schema-${view.name}`}
                title={view.description}
                className="flex flex-1 items-center gap-1 font-mono"
                onClick={() => setExpanded((previous) => toggled(previous, view.name))}
              >
                <ChevronRight className={open ? "size-3 rotate-90" : "size-3"} />
                {view.name}
              </button>
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label={`Insert ${view.name}`}
                onClick={() => onInsert(view.name)}
              >
                <Plus />
              </Button>
            </div>
            {open ? (
              <ul id={`schema-${view.name}`} aria-label={`${view.name} columns`}>
                {view.columns.map((column) => (
                  <li key={column.name}>
                    <button
                      type="button"
                      className="flex w-full justify-between gap-2 rounded px-1 hover:bg-muted"
                      title={column.description}
                      aria-label={`Insert ${column.name}`}
                      onClick={() => onInsert(column.name)}
                    >
                      <span className="font-mono">{column.name}</span>
                      <span className="text-muted-foreground">{column.column_type}</span>
                    </button>
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        );
      })}
      <div key="functions">
        <div className="flex items-center gap-1">
          <button
            type="button"
            aria-expanded={expanded.has("functions")}
            aria-controls="schema-functions"
            className="flex flex-1 items-center gap-1 font-mono"
            onClick={() => setExpanded((previous) => toggled(previous, "functions"))}
          >
            <ChevronRight className={expanded.has("functions") ? "size-3 rotate-90" : "size-3"} />
            Functions
          </button>
        </div>
        {expanded.has("functions") ? (
          <ul id="schema-functions" aria-label="Functions">
            {schema.functions
              .filter((fn) => fn.in_sql)
              .map((fn) => (
                <li key={fn.name}>
                  <button
                    type="button"
                    className="flex w-full justify-between gap-2 rounded px-1 hover:bg-muted"
                    title={fn.signature}
                    aria-label={`Insert ${fn.name}(`}
                    onClick={() => onInsert(`${fn.name}(`)}
                  >
                    <span className="font-mono">{fn.name}</span>
                  </button>
                </li>
              ))}
          </ul>
        ) : null}
      </div>
    </nav>
  );
}
