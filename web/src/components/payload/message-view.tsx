import { cn } from "cn";
import { type ReactNode, useState } from "react";
import type { Schema } from "@/api/types";
import { PartView, type ToolLinks } from "@/components/payload/part-view";
import { Badge } from "@/components/ui/badge";

const LINE_LIMIT = 40;

export function MessageView({
  message,
  projectRef,
  links,
}: {
  message: Schema<"Message">;
  projectRef?: string;
  links: ToolLinks;
}) {
  const [expanded, setExpanded] = useState(false);
  const total = textLineCount(message);
  const hidden = total - LINE_LIMIT;
  const role = message.role.length > 0 ? message.role : "other";
  return (
    <article className={cn("border-l-2 pl-2", roleBorder(role))}>
      <header className="flex flex-wrap items-center gap-2 text-xs">
        <span className="font-medium">{role}</span>
        {message.name ? <span className="text-muted-foreground">{message.name}</span> : null}
        {message.finish_reason ? (
          <Badge variant="outline" className="text-muted-foreground">
            {message.finish_reason}
          </Badge>
        ) : null}
      </header>
      <div className="flex flex-col gap-1">
        {partsOf(message, expanded, projectRef, links)}
        {!expanded && hidden > 0 ? (
          <button type="button" className="self-start text-xs" onClick={() => setExpanded(true)}>
            Show {hidden} more lines
          </button>
        ) : null}
      </div>
    </article>
  );
}

function partsOf(
  message: Schema<"Message">,
  expanded: boolean,
  projectRef: string | undefined,
  links: ToolLinks,
): ReactNode[] {
  const nodes: ReactNode[] = [];
  let budget = expanded ? Number.POSITIVE_INFINITY : LINE_LIMIT;
  const seen = new Map<string, number>();
  for (const part of message.parts) {
    const key = partKey(part, seen);
    if (part.type === "text" && typeof part.content === "string") {
      if (budget <= 0) continue;
      const lines = part.content.split("\n");
      const shown = lines.slice(0, budget).join("\n");
      budget -= lines.length;
      nodes.push(
        <PartView
          key={key}
          part={{ type: "text", content: shown }}
          projectRef={projectRef}
          links={links}
        />,
      );
      continue;
    }
    nodes.push(<PartView key={key} part={part} projectRef={projectRef} links={links} />);
  }
  return nodes;
}

function textLineCount(message: Schema<"Message">): number {
  let count = 0;
  for (const part of message.parts) {
    if (part.type === "text" && typeof part.content === "string") {
      count += part.content.split("\n").length;
    }
  }
  return count;
}

function partKey(part: Schema<"Part">, seen: Map<string, number>): string {
  const id = "id" in part && typeof part.id === "string" ? part.id : "";
  const content =
    "content" in part && typeof part.content === "string" ? part.content.slice(0, 24) : "";
  const base = `${part.type ?? "part"}\0${id}\0${content}`;
  const n = seen.get(base) ?? 0;
  seen.set(base, n + 1);
  return `${base}\0${n}`;
}

function roleBorder(role: string): string {
  if (role === "system") return "border-muted-foreground";
  if (role === "user") return "border-kind-span";
  if (role === "assistant") return "border-kind-llm";
  if (role === "tool") return "border-kind-tool";
  return "border-kind-agent";
}
