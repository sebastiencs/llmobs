import { useState } from "react";
import type { Schema } from "@/api/types";
import { CopyButton } from "@/components/copy-button";
import { FormatSwitch, usePayloadFormat } from "@/components/payload/format-switch";
import { JsonView } from "@/components/payload/json-view";
import { Badge } from "@/components/ui/badge";

/** Pretty rendering stops here until the user asks for it. Serialized characters, not bytes. */
const RENDER_CAP = 1_000_000;

export function PayloadView({
  payload,
  label,
}: {
  payload: Schema<"Payload"> | undefined;
  label: string;
}) {
  const [mode, setMode] = usePayloadFormat();
  const [force, setForce] = useState(false);
  const [seen, setSeen] = useState(payload);
  if (seen !== payload) {
    setSeen(payload);
    setForce(false);
  }
  if (!payload) {
    return (
      <section className="flex min-w-0 flex-col gap-1">
        <h3 className="text-xs font-medium">{label}</h3>
        <p className="text-xs text-muted-foreground">—</p>
      </section>
    );
  }

  const serialized = serializedPayload(payload);
  const capped = serialized.chars > RENDER_CAP && mode === "pretty" && !force;
  return (
    <section className="flex min-w-0 flex-col gap-1">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-xs font-medium">{label}</h3>
        <Badge variant="outline" className="h-4 px-1.5 text-[10px] text-muted-foreground">
          {payload.format}
        </Badge>
        <span className="text-xs text-muted-foreground">{serialized.chars} chars</span>
        <CopyButton value={serialized.raw} label={label} />
        <FormatSwitch value={mode} onChange={setMode} />
      </div>
      {capped ? (
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <p>Large payload ({megabytes(serialized.chars)} MB)</p>
          <button type="button" className="underline" onClick={() => setForce(true)}>
            Render anyway
          </button>
        </div>
      ) : (
        <PayloadBody payload={payload} mode={mode} serialized={serialized} />
      )}
    </section>
  );
}

function PayloadBody({
  payload,
  mode,
  serialized,
}: {
  payload: Schema<"Payload">;
  mode: "pretty" | "json" | "raw";
  serialized: Serialized;
}) {
  if (payload.format === "text") {
    return (
      <pre className="overflow-auto font-mono text-xs whitespace-pre-wrap">{payload.value}</pre>
    );
  }
  if (payload.format === "messages" && mode === "pretty") {
    return <MessageList messages={payload.value} />;
  }
  if (mode === "raw") {
    return (
      <pre className="overflow-auto font-mono text-xs whitespace-pre-wrap">{serialized.raw}</pre>
    );
  }
  if (payload.format === "json" && mode === "json") {
    return (
      <pre className="overflow-auto font-mono text-xs whitespace-pre-wrap">{serialized.pretty}</pre>
    );
  }
  return <JsonView value={payload.value} />;
}

function MessageList({ messages }: { messages: readonly Schema<"Message">[] }) {
  return (
    <ul className="flex flex-col gap-2">
      {messages.map((message) => (
        <li key={messageKey(message)} className="text-xs">
          <span className="font-medium">{message.role}</span>
          <p className="whitespace-pre-wrap">{messageText(message)}</p>
        </li>
      ))}
    </ul>
  );
}

function messageKey(message: Schema<"Message">): string {
  const name = message.name ?? "";
  return `${message.role}\0${name}\0${messageText(message)}`;
}

function messageText(message: Schema<"Message">): string {
  const lines: string[] = [];
  for (const part of message.parts) {
    if (part.type === "text" && typeof part.content === "string") {
      lines.push(part.content);
    }
  }
  return lines.join("\n");
}

type Serialized = { pretty: string; raw: string; chars: number };

function serializedPayload(payload: Schema<"Payload">): Serialized {
  if (payload.format === "text") {
    return { pretty: payload.value, raw: payload.value, chars: payload.value.length };
  }
  const raw = JSON.stringify(payload.value) ?? "";
  const pretty = JSON.stringify(payload.value, null, 2) ?? "";
  return { pretty, raw, chars: raw.length };
}

function megabytes(chars: number): string {
  const mb = chars / 1_000_000;
  return mb >= 10 ? mb.toFixed(0) : mb.toFixed(1);
}
