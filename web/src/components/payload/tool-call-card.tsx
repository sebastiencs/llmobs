import { IdText } from "@/components/id-text";
import { JsonView } from "@/components/payload/json-view";

export function ToolCallCard({
  anchorId,
  title,
  titleTargetId,
  id,
  body,
  forwardTargetId,
}: {
  anchorId?: string;
  title: string;
  titleTargetId?: string;
  id?: string;
  body: unknown;
  forwardTargetId?: string;
}) {
  return (
    <article id={anchorId} className="flex flex-col gap-1 rounded border px-2 py-1">
      <header className="flex flex-wrap items-center gap-2 text-xs">
        {titleTargetId ? (
          <button type="button" className="font-medium" onClick={() => scrollTo(titleTargetId)}>
            {title}
          </button>
        ) : (
          <span className="font-medium">{title}</span>
        )}
        {id ? <IdText value={id} kind="call" /> : null}
        {forwardTargetId ? (
          <button type="button" onClick={() => scrollTo(forwardTargetId)}>
            → response
          </button>
        ) : null}
      </header>
      <ArgumentBody value={body} />
    </article>
  );
}

function ArgumentBody({ value }: { value: unknown }) {
  if (value === undefined) return null;
  if (typeof value === "string") {
    const parsed = jsonObject(value);
    if (parsed === undefined) {
      return <p className="text-xs break-words whitespace-pre-wrap">{value}</p>;
    }
    return <JsonView value={parsed} />;
  }
  return <JsonView value={value} />;
}

function jsonObject(value: string): unknown | undefined {
  try {
    const parsed: unknown = JSON.parse(value);
    if (parsed !== null && typeof parsed === "object") return parsed;
  } catch {
    return undefined;
  }
  return undefined;
}

function scrollTo(id: string) {
  document.getElementById(id)?.scrollIntoView({ block: "nearest" });
}
