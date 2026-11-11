import { useState } from "react";
import type { Schema } from "@/api/types";
import { JsonView } from "@/components/payload/json-view";
import { MediaView } from "@/components/payload/media-view";
import { ToolCallCard } from "@/components/payload/tool-call-card";

export type ToolLinks = {
  callNames: Map<string, string>;
  responseIds: Set<string>;
};

export function PartView({
  part,
  projectRef,
  links,
}: {
  part: Schema<"Part">;
  projectRef?: string;
  links: ToolLinks;
}) {
  if (part.type === "text" && typeof part.content === "string") {
    return <p className="text-xs break-words whitespace-pre-wrap">{part.content}</p>;
  }
  if (part.type === "reasoning" && typeof part.content === "string") {
    return <Reasoning content={part.content} />;
  }
  if (part.type === "tool_call") {
    const id = typeof part.id === "string" ? part.id : undefined;
    const name = typeof part.name === "string" ? part.name : "tool";
    return (
      <ToolCallCard
        anchorId={id ? `tool-call-${id}` : undefined}
        title={name}
        id={id}
        body={part.arguments}
        forwardTargetId={id && links.responseIds.has(id) ? `tool-response-${id}` : undefined}
      />
    );
  }
  if (part.type === "tool_call_response") {
    const id = typeof part.id === "string" ? part.id : undefined;
    const callName = id ? links.callNames.get(id) : undefined;
    return (
      <ToolCallCard
        anchorId={id ? `tool-response-${id}` : undefined}
        title={callName ?? "Response"}
        titleTargetId={id && callName ? `tool-call-${id}` : undefined}
        id={id}
        body={part.response}
      />
    );
  }
  if (isUri(part) || isBlob(part)) {
    return <MediaView part={part} projectRef={projectRef} />;
  }
  return (
    <div className="flex flex-col gap-1">
      <p className="text-xs text-muted-foreground">Unknown part</p>
      <JsonView value={part} />
    </div>
  );
}

function isUri(part: Schema<"Part">): part is Schema<"UriPart"> {
  return part.type === "uri" && typeof part.uri === "string";
}

function isBlob(part: Schema<"Part">): part is Schema<"BlobPart"> {
  return part.type === "blob" && typeof part.content === "string";
}

function Reasoning({ content }: { content: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="flex flex-col gap-1">
      <button
        type="button"
        className="self-start text-xs text-muted-foreground"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        Reasoning · {content.length}
      </button>
      {open ? (
        <p className="text-xs break-words whitespace-pre-wrap text-muted-foreground italic">
          {content}
        </p>
      ) : null}
    </div>
  );
}
