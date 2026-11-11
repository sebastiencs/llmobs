import { useVirtualizer } from "@tanstack/react-virtual";
import { cn } from "cn";
import { ChevronDown, ChevronRight } from "lucide-react";
import { type ReactNode, useMemo, useRef, useState } from "react";
import { CopyButton } from "@/components/copy-button";
import { MediaChip } from "@/components/payload/media-chip";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  ancestorPaths,
  defaultCollapsedPaths,
  type JsonRow,
  jsonRows,
  visibleJsonRows,
} from "@/lib/json-rows";

const ROW_HEIGHT = 22;
const VIRTUAL_THRESHOLD = 300;
const STRING_LIMIT = 200;

export function JsonView({
  value,
  defaultExpandDepth = 2,
}: {
  value: unknown;
  defaultExpandDepth?: number;
}) {
  const rows = useMemo(() => jsonRows(value), [value]);
  const [collapsed, setCollapsed] = useState(() => defaultCollapsedPaths(rows, defaultExpandDepth));
  const [query, setQuery] = useState("");
  const [openStrings, setOpenStrings] = useState<ReadonlySet<string>>(() => new Set());
  const [seen, setSeen] = useState(rows);
  if (seen !== rows) {
    setSeen(rows);
    setCollapsed(defaultCollapsedPaths(rows, defaultExpandDepth));
    setQuery("");
    setOpenStrings(new Set());
  }

  const needle = query.trim().toLowerCase();
  const forced = useMemo(() => {
    const open = new Set<string>();
    if (!needle) {
      return open;
    }
    for (const row of rows) {
      if (!rowMatches(row, needle)) {
        continue;
      }
      for (const path of ancestorPaths(row.path)) {
        open.add(path);
      }
    }
    return open;
  }, [rows, needle]);
  const effective = useMemo(() => {
    if (forced.size === 0) {
      return collapsed;
    }
    const next = new Set(collapsed);
    for (const path of forced) {
      next.delete(path);
    }
    return next;
  }, [collapsed, forced]);
  const visible = useMemo(() => visibleJsonRows(rows, effective), [rows, effective]);
  const virtual = visible.length > VIRTUAL_THRESHOLD;
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: virtual ? visible.length : 0,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    measureElement: (element) => {
      const height = element.getBoundingClientRect().height;
      return height > 0 ? height : ROW_HEIGHT;
    },
    overscan: 12,
  });

  const pretty = useMemo(() => JSON.stringify(value, null, 2) ?? "", [value]);

  function toggle(path: string) {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }

  function toggleString(path: string) {
    setOpenStrings((current) => {
      const next = new Set(current);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  }

  function rowAt(index: number): JsonRow | undefined {
    const rowIndex = visible[index];
    return rowIndex === undefined ? undefined : rows[rowIndex];
  }

  return (
    <div className="flex min-w-0 flex-col gap-1">
      <div className="flex flex-wrap items-center gap-1">
        <Button type="button" variant="ghost" size="xs" onClick={() => setCollapsed(new Set())}>
          Expand all
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="xs"
          onClick={() =>
            setCollapsed(new Set(rows.filter((row) => row.childCount > 0).map((row) => row.path)))
          }
        >
          Collapse all
        </Button>
        <CopyButton value={pretty} label="JSON" />
        <Input
          value={query}
          aria-label="Search JSON"
          placeholder="Search"
          className="h-6 w-40 text-xs"
          onChange={(event) => setQuery(event.target.value)}
        />
      </div>
      {virtual ? (
        <div ref={scrollRef} data-scroll-container className="h-80 overflow-auto">
          <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((item) => {
              const row = rowAt(item.index);
              if (!row) {
                return null;
              }
              return (
                <div
                  key={row.path || "root"}
                  ref={virtualizer.measureElement}
                  data-index={item.index}
                  className="absolute top-0 w-full"
                  style={{ transform: `translateY(${item.start}px)` }}
                >
                  <JsonRowView
                    row={row}
                    collapsed={effective.has(row.path)}
                    query={needle}
                    stringOpen={openStrings.has(row.path)}
                    onToggle={toggle}
                    onToggleString={toggleString}
                  />
                </div>
              );
            })}
          </div>
        </div>
      ) : (
        <div>
          {visible.map((index) => {
            const row = rows[index];
            if (!row) {
              return null;
            }
            return (
              <JsonRowView
                key={row.path || "root"}
                row={row}
                collapsed={effective.has(row.path)}
                query={needle}
                stringOpen={openStrings.has(row.path)}
                onToggle={toggle}
                onToggleString={toggleString}
              />
            );
          })}
        </div>
      )}
    </div>
  );
}

function JsonRowView({
  row,
  collapsed,
  query,
  stringOpen,
  onToggle,
  onToggleString,
}: {
  row: JsonRow;
  collapsed: boolean;
  query: string;
  stringOpen: boolean;
  onToggle: (path: string) => void;
  onToggleString: (path: string) => void;
}) {
  const container = row.kind === "object" || row.kind === "array";
  const expanded = container && row.childCount > 0 && !collapsed;
  const label = row.key || "value";
  return (
    <div
      className={cn(
        "flex min-h-[22px] min-w-0 gap-1 font-mono text-xs",
        stringOpen ? "items-start" : "items-center",
      )}
      style={{ paddingLeft: row.depth * 12 }}
    >
      {container && row.childCount > 0 ? (
        <button
          type="button"
          className="inline-flex size-3.5 shrink-0 items-center justify-center text-muted-foreground"
          aria-expanded={!collapsed}
          aria-label={`${collapsed ? "Expand" : "Collapse"} ${label}`}
          onClick={() => onToggle(row.path)}
        >
          {collapsed ? <ChevronRight className="size-3" /> : <ChevronDown className="size-3" />}
        </button>
      ) : (
        <span className="inline-block size-3.5 shrink-0" />
      )}
      {row.key ? (
        <span className="shrink-0 text-muted-foreground">
          <Highlight text={row.key} query={query} />
        </span>
      ) : null}
      {container && !expanded ? <span className="text-muted-foreground">{row.preview}</span> : null}
      {container ? null : (
        <JsonPrimitive
          row={row}
          query={query}
          open={stringOpen}
          onToggle={() => onToggleString(row.path)}
        />
      )}
    </div>
  );
}

function JsonPrimitive({
  row,
  query,
  open,
  onToggle,
}: {
  row: JsonRow;
  query: string;
  open: boolean;
  onToggle: () => void;
}) {
  if (row.kind === "string" && row.preview.startsWith("media://")) {
    return <MediaChip uri={row.preview} />;
  }
  if (row.kind === "string" && row.preview.length > STRING_LIMIT && !open) {
    const shown = `${row.preview.slice(0, STRING_LIMIT)}… (${row.preview.length} chars)`;
    return (
      <button
        type="button"
        className="min-w-0 text-left break-all text-kind-tool"
        onClick={onToggle}
      >
        <Highlight text={shown} query={query} />
      </button>
    );
  }
  return (
    <span className={cn("min-w-0 break-all", kindClass(row.kind), open && "whitespace-pre-wrap")}>
      <Highlight text={row.preview} query={query} />
    </span>
  );
}

function Highlight({ text, query }: { text: string; query: string }): ReactNode {
  if (!query) {
    return text;
  }
  const lower = text.toLowerCase();
  const parts: ReactNode[] = [];
  let cursor = 0;
  while (cursor < text.length) {
    const at = lower.indexOf(query, cursor);
    if (at < 0) {
      parts.push(text.slice(cursor));
      break;
    }
    if (at > cursor) {
      parts.push(text.slice(cursor, at));
    }
    parts.push(<mark key={at}>{text.slice(at, at + query.length)}</mark>);
    cursor = at + query.length;
  }
  return parts;
}

function rowMatches(row: JsonRow, needle: string): boolean {
  if (row.key.toLowerCase().includes(needle)) {
    return true;
  }
  return (
    row.kind !== "object" && row.kind !== "array" && row.preview.toLowerCase().includes(needle)
  );
}

function kindClass(kind: JsonRow["kind"]): string {
  if (kind === "string") {
    return "text-kind-tool";
  }
  if (kind === "number") {
    return "text-kind-span";
  }
  if (kind === "boolean") {
    return "text-kind-llm";
  }
  return "text-muted-foreground";
}
