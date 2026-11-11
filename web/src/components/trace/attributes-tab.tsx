import { Plus } from "lucide-react";
import { type ReactNode, useState } from "react";
import type { Schema } from "@/api/types";
import { CopyButton } from "@/components/copy-button";
import { JsonView } from "@/components/payload/json-view";
import {
  attributeEntries,
  EventsList,
  LinksList,
  recordOf,
  valueText,
} from "@/components/trace/events-list";
import { Input } from "@/components/ui/input";
import { serializeChip } from "@/lib/dsl/dsl";

type MapColumn = "metadata" | "attributes";

export function AttributesTab({
  span,
  onAddFilter,
}: {
  span: Schema<"SpanDetail">;
  onAddFilter?: (conjunct: string) => void;
}) {
  const [query, setQuery] = useState("");
  const needle = query.trim().toLowerCase();
  return (
    <div className="flex flex-col gap-3">
      <Input
        value={query}
        aria-label="Search attributes"
        placeholder="Search"
        className="h-7 text-xs"
        onChange={(event) => setQuery(event.target.value)}
      />
      <MapSection
        title="Metadata"
        values={span.metadata}
        query={needle}
        column="metadata"
        onAddFilter={onAddFilter}
      />
      <MapSection
        title="Attributes"
        values={span.attributes}
        query={needle}
        column="attributes"
        onAddFilter={onAddFilter}
      />
      <MapSection title="Resource attributes" values={span.resource_attributes} query={needle} />
      <ParamsSection value={span.request_params} query={needle} />
      <EventsList events={span.events} startTime={span.start_time} query={needle} />
      <LinksList links={span.links} query={needle} />
    </div>
  );
}

function MapSection({
  title,
  values,
  query,
  column,
  onAddFilter,
}: {
  title: string;
  values: Record<string, string>;
  query: string;
  column?: MapColumn;
  onAddFilter?: (conjunct: string) => void;
}) {
  const rows = attributeEntries(values).filter((row) => matches(row.key, row.value, query));
  return (
    <Section title={title} count={rows.length}>
      <RowList label={title}>
        {rows.map((row) => (
          <KvRow
            key={row.key}
            name={row.key}
            value={row.value}
            conjunct={
              column && typeof row.value === "string"
                ? mapConjunct(column, row.key, row.value)
                : undefined
            }
            onAddFilter={onAddFilter}
          />
        ))}
      </RowList>
    </Section>
  );
}

function ParamsSection({ value, query }: { value: unknown; query: string }) {
  const rows = attributeEntries(recordOf(value)).filter((row) =>
    matches(row.key, row.value, query),
  );
  return (
    <Section title="Request params" count={rows.length}>
      <RowList label="Request params">
        {rows.map((row) => (
          <KvRow key={row.key} name={row.key} value={row.value} />
        ))}
      </RowList>
    </Section>
  );
}

function Section({
  title,
  count,
  children,
}: {
  title: string;
  count: number;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(true);
  return (
    <section className="flex flex-col">
      <button
        type="button"
        className="flex h-6 items-center gap-1 text-left text-xs font-medium"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        {title}
        <span className="text-muted-foreground">{count}</span>
      </button>
      {open ? children : null}
    </section>
  );
}

function RowList({ label, children }: { label: string; children: ReactNode }) {
  return <ul aria-label={label}>{children}</ul>;
}

function KvRow({
  name,
  value,
  conjunct,
  onAddFilter,
}: {
  name: string;
  value: unknown;
  conjunct?: string;
  onAddFilter?: (conjunct: string) => void;
}) {
  const text = valueText(value);
  const json = jsonContainer(value);
  return (
    <li className="grid min-h-6 grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)_auto] items-center gap-2 border-b">
      <span className="truncate font-mono text-xs">{name}</span>
      {json === undefined ? (
        <span className="truncate font-mono text-xs">{text}</span>
      ) : (
        <JsonView value={json} />
      )}
      <span className="flex items-center">
        {conjunct && onAddFilter ? (
          <button
            type="button"
            aria-label={`Filter by ${conjunct}`}
            className="inline-flex size-4 items-center justify-center text-muted-foreground"
            onClick={() => onAddFilter(conjunct)}
          >
            <Plus className="size-3" />
          </button>
        ) : null}
        <CopyButton value={text} label={name} />
      </span>
    </li>
  );
}

function matches(key: string, value: unknown, query: string): boolean {
  if (!query) {
    return true;
  }
  return key.toLowerCase().includes(query) || valueText(value).toLowerCase().includes(query);
}

function jsonContainer(value: unknown): unknown {
  if (typeof value === "string") {
    try {
      const parsed: unknown = JSON.parse(value);
      if (parsed !== null && typeof parsed === "object") {
        return parsed;
      }
    } catch {
      return undefined;
    }
    return undefined;
  }
  if (value !== null && typeof value === "object") {
    return value;
  }
  return undefined;
}

function mapConjunct(column: MapColumn, key: string, value: string): string {
  return serializeChip({
    kind: "compare",
    field: { column, key },
    op: "=",
    value: { kind: "string", value },
  });
}
