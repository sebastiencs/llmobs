import { Link } from "@tanstack/react-router";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";
import type { Schema } from "@/api/types";
import { IdText } from "@/components/id-text";
import { useProject } from "@/components/shell/project-context";
import { formatDuration, nsBetween } from "@/lib/format";

const STACK_COLLAPSE_LINES = 12;

export function EventsList({
  events,
  startTime,
  query,
}: {
  events: readonly Schema<"SpanEventView">[];
  startTime: string;
  query: string;
}) {
  const visible = events.filter((event) => eventMatches(event, query));
  if (events.length === 0) {
    return null;
  }
  return (
    <section className="flex flex-col gap-1" aria-label="Events">
      <h3 className="text-xs font-medium">
        Events <span className="text-muted-foreground">{visible.length}</span>
      </h3>
      {visible.map((event) => (
        <EventRow key={`${event.time}-${event.name}`} event={event} startTime={startTime} />
      ))}
    </section>
  );
}

function EventRow({ event, startTime }: { event: Schema<"SpanEventView">; startTime: string }) {
  const attributes = recordOf(event.attributes);
  const type = textOf(attributes?.["exception.type"]);
  const message = textOf(attributes?.["exception.message"]);
  const stack = textOf(attributes?.["exception.stacktrace"]);
  const rest = attributeEntries(attributes).filter(
    (entry) =>
      entry.key !== "exception.type" &&
      entry.key !== "exception.message" &&
      entry.key !== "exception.stacktrace",
  );
  return (
    <article className="border-b py-1 text-xs">
      <div className="flex min-w-0 items-center gap-2">
        <span className="num shrink-0 text-muted-foreground">
          {offsetLabel(startTime, event.time)}
        </span>
        <span className="min-w-0 truncate font-medium">{event.name}</span>
      </div>
      {event.name === "exception" ? (
        <p className="mt-1">
          {type ? <span className="font-medium">{type}</span> : null}
          {type && message ? ": " : null}
          {message}
        </p>
      ) : null}
      {stack ? <Stacktrace text={stack} /> : null}
      {rest.length > 0 ? (
        <ul className="mt-1">
          {rest.map((entry) => (
            <li key={entry.key} className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)] gap-2">
              <span className="truncate font-mono">{entry.key}</span>
              <ValueText value={entry.value} />
            </li>
          ))}
        </ul>
      ) : null}
    </article>
  );
}

function Stacktrace({ text }: { text: string }) {
  const long = text.split("\n").length > STACK_COLLAPSE_LINES;
  const [open, setOpen] = useState(!long);
  return (
    <div className="mt-1">
      <button
        type="button"
        className="inline-flex items-center gap-1 text-muted-foreground"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
        {open ? "Hide stacktrace" : "Show stacktrace"}
      </button>
      {open ? <pre className="mt-1 overflow-auto font-mono whitespace-pre">{text}</pre> : null}
    </div>
  );
}

export function LinksList({
  links,
  query,
}: {
  links: readonly Schema<"SpanLinkView">[];
  query: string;
}) {
  const { org, project } = useProject();
  const visible = links.filter((link) => linkMatches(link, query));
  if (links.length === 0) {
    return null;
  }
  return (
    <section className="flex flex-col gap-1" aria-label="Links">
      <h3 className="text-xs font-medium">
        Links <span className="text-muted-foreground">{visible.length}</span>
      </h3>
      {visible.map((link) => {
        const attributes = attributeEntries(recordOf(link.attributes));
        return (
          <article key={`${link.trace_id}-${link.span_id}`} className="border-b py-1 text-xs">
            <div className="flex flex-wrap items-center gap-2">
              <IdText value={link.trace_id} kind="trace" />
              <IdText value={link.span_id} kind="span" />
              <Link
                to="/$org/$project/traces"
                params={{ org, project }}
                search={{ trace: link.trace_id, span: link.span_id }}
                className="focus-ring rounded-sm underline"
              >
                Open trace
              </Link>
            </div>
            {attributes.length > 0 ? (
              <ul className="mt-1">
                {attributes.map((entry) => (
                  <li
                    key={entry.key}
                    className="grid grid-cols-[minmax(0,1fr)_minmax(0,1.4fr)] gap-2"
                  >
                    <span className="truncate font-mono">{entry.key}</span>
                    <ValueText value={entry.value} />
                  </li>
                ))}
              </ul>
            ) : null}
          </article>
        );
      })}
    </section>
  );
}

function offsetLabel(start: string, time: string): string {
  const delta = nsBetween(start, time);
  if (delta < 0) {
    return `-${formatDuration(-delta)}`;
  }
  return formatDuration(delta);
}

function eventMatches(event: Schema<"SpanEventView">, query: string): boolean {
  if (!query) {
    return true;
  }
  if (event.name.toLowerCase().includes(query)) {
    return true;
  }
  return attributeEntries(recordOf(event.attributes)).some((entry) => entryMatches(entry, query));
}

function linkMatches(link: Schema<"SpanLinkView">, query: string): boolean {
  if (!query) {
    return true;
  }
  if (link.trace_id.toLowerCase().includes(query) || link.span_id.toLowerCase().includes(query)) {
    return true;
  }
  return attributeEntries(recordOf(link.attributes)).some((entry) => entryMatches(entry, query));
}

function entryMatches(entry: { key: string; value: unknown }, query: string): boolean {
  return (
    entry.key.toLowerCase().includes(query) || valueText(entry.value).toLowerCase().includes(query)
  );
}

export function recordOf(value: unknown): Record<string, unknown> | undefined {
  if (typeof value === "string") {
    try {
      return recordOf(JSON.parse(value));
    } catch {
      return undefined;
    }
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return undefined;
  }
  return value as Record<string, unknown>;
}

export function attributeEntries(
  attributes: Record<string, unknown> | undefined,
): { key: string; value: unknown }[] {
  if (!attributes) {
    return [];
  }
  return Object.entries(attributes)
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([key, value]) => ({ key, value }));
}

function textOf(value: unknown): string | undefined {
  if (typeof value !== "string" || value.length === 0) {
    return undefined;
  }
  return value;
}

function ValueText({ value }: { value: unknown }) {
  const text = valueText(value);
  const json = isJsonContainer(value) || parsedContainer(value) !== undefined;
  if (json) {
    return <pre className="overflow-auto font-mono whitespace-pre-wrap">{text}</pre>;
  }
  return <span className="truncate font-mono">{text}</span>;
}

export function valueText(value: unknown): string {
  const parsed = typeof value === "string" ? parsedContainer(value) : undefined;
  if (parsed !== undefined) {
    return JSON.stringify(parsed, null, 2);
  }
  if (isJsonContainer(value)) {
    return JSON.stringify(value, null, 2);
  }
  if (value === undefined || value === null) {
    return "";
  }
  return String(value);
}

function parsedContainer(value: unknown): unknown {
  if (typeof value !== "string") {
    return undefined;
  }
  try {
    const parsed: unknown = JSON.parse(value);
    return isJsonContainer(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}

function isJsonContainer(value: unknown): boolean {
  return typeof value === "object" && value !== null;
}
