import { useState } from "react";
import type { Schema } from "@/api/types";
import { JsonView } from "@/components/payload/json-view";
import { MessageView } from "@/components/payload/message-view";
import type { ToolLinks } from "@/components/payload/part-view";

export function ChatView({
  input,
  output,
  systemInstructions,
  toolDefinitions,
  projectRef,
}: {
  input: readonly Schema<"Message">[];
  output: readonly Schema<"Message">[];
  systemInstructions?: unknown;
  toolDefinitions?: unknown;
  projectRef?: string;
}) {
  const messages = [...input, ...output];
  const links = toolLinks(messages);
  const tools = toolDefs(toolDefinitions);
  return (
    <div className="flex flex-col gap-3">
      <SystemInstructions value={systemInstructions} />
      <Tools tools={tools} called={calledNames(messages)} />
      <MessageList messages={input} projectRef={projectRef} links={links} />
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span className="h-px flex-1 bg-border" />
        <span>Output</span>
        <span className="h-px flex-1 bg-border" />
      </div>
      <MessageList messages={output} projectRef={projectRef} links={links} />
    </div>
  );
}

function MessageList({
  messages,
  projectRef,
  links,
}: {
  messages: readonly Schema<"Message">[];
  projectRef?: string;
  links: ToolLinks;
}) {
  const seen = new Map<string, number>();
  return (
    <div className="flex flex-col gap-2">
      {messages.map((message) => (
        <MessageView
          key={messageKey(message, seen)}
          message={message}
          projectRef={projectRef}
          links={links}
        />
      ))}
    </div>
  );
}

function SystemInstructions({ value }: { value: unknown }) {
  const text = instructionText(value);
  const [open, setOpen] = useState(false);
  if (!text) return null;
  const lines = text.split("\n");
  const count = lines.length;
  const label = count === 1 ? "1 line" : `${count} lines`;
  return (
    <section className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        <button
          type="button"
          className="text-xs font-medium"
          aria-expanded={open}
          onClick={() => setOpen((current) => !current)}
        >
          System instructions
        </button>
        <span className="text-xs text-muted-foreground">{label}</span>
      </div>
      {open ? (
        <p className="text-xs break-words whitespace-pre-wrap">{text}</p>
      ) : (
        <p className="truncate text-xs">{lines[0] ?? ""}</p>
      )}
    </section>
  );
}

function Tools({ tools, called }: { tools: ToolDef[]; called: Set<string> }) {
  const [open, setOpen] = useState(false);
  if (tools.length === 0) return null;
  const seen = new Map<string, number>();
  return (
    <section className="flex flex-col gap-1">
      <button
        type="button"
        className="self-start text-xs font-medium"
        aria-expanded={open}
        onClick={() => setOpen((current) => !current)}
      >
        Tools ({tools.length})
      </button>
      {open ? (
        <ul className="flex flex-col gap-2">
          {tools.map((tool) => (
            <li key={toolKey(tool.name, seen)} className="flex flex-col gap-1">
              <div className="text-xs">
                {called.has(tool.name) ? (
                  <mark className="rounded bg-kind-tool/30 px-0.5">{tool.name}</mark>
                ) : (
                  <span>{tool.name}</span>
                )}
              </div>
              {tool.description ? (
                <p className="text-xs text-muted-foreground">{tool.description}</p>
              ) : null}
              {tool.schema !== undefined ? <JsonView value={tool.schema} /> : null}
            </li>
          ))}
        </ul>
      ) : null}
    </section>
  );
}

type ToolDef = { name: string; description?: string; schema?: unknown };

function toolLinks(messages: readonly Schema<"Message">[]): ToolLinks {
  const callNames = new Map<string, string>();
  const responseIds = new Set<string>();
  for (const message of messages) {
    for (const part of message.parts) {
      if (
        part.type === "tool_call" &&
        typeof part.id === "string" &&
        typeof part.name === "string"
      ) {
        callNames.set(part.id, part.name);
      }
      if (part.type === "tool_call_response" && typeof part.id === "string") {
        responseIds.add(part.id);
      }
    }
  }
  return { callNames, responseIds };
}

function calledNames(messages: readonly Schema<"Message">[]): Set<string> {
  const names = new Set<string>();
  for (const message of messages) {
    for (const part of message.parts) {
      if (part.type === "tool_call" && typeof part.name === "string") names.add(part.name);
    }
  }
  return names;
}

function instructionText(value: unknown): string {
  const data = parsed(value);
  if (typeof data === "string") return data;
  if (Array.isArray(data)) {
    const lines: string[] = [];
    for (const item of data) {
      const text = textOf(item);
      if (text !== undefined) lines.push(text);
    }
    return lines.join("\n");
  }
  return textOf(data) ?? "";
}

function textOf(value: unknown): string | undefined {
  if (typeof value === "string") return value;
  if (
    value &&
    typeof value === "object" &&
    "content" in value &&
    typeof value.content === "string"
  ) {
    return value.content;
  }
  return undefined;
}

function toolDefs(value: unknown): ToolDef[] {
  const data = parsed(value);
  if (!Array.isArray(data)) return [];
  const tools: ToolDef[] = [];
  for (const item of data) {
    if (!item || typeof item !== "object") continue;
    const record = item as Record<string, unknown>;
    const fn = record.function;
    const source = fn && typeof fn === "object" ? (fn as Record<string, unknown>) : record;
    if (typeof source.name !== "string") continue;
    const description = typeof source.description === "string" ? source.description : undefined;
    const schema = source.parameters ?? source.input_schema;
    tools.push({ name: source.name, description, schema });
  }
  return tools;
}

function parsed(value: unknown): unknown {
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value) as unknown;
  } catch {
    return value;
  }
}

function messageKey(message: Schema<"Message">, seen: Map<string, number>): string {
  const text = message.parts
    .map((part) =>
      part.type === "text" && typeof part.content === "string" ? part.content : part.type,
    )
    .join("\0");
  const base = `${message.role}\0${message.name ?? ""}\0${text}`;
  const n = seen.get(base) ?? 0;
  seen.set(base, n + 1);
  return `${base}\0${n}`;
}

function toolKey(name: string, seen: Map<string, number>): string {
  const n = seen.get(name) ?? 0;
  seen.set(name, n + 1);
  return `${name}\0${n}`;
}
