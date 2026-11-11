import type { Schema } from "@/api/types";
import { ChatView } from "@/components/payload/chat-view";
import { useProject } from "@/components/shell/project-context";

export function ChatTab({ span }: { span: Schema<"SpanDetail"> }) {
  const { ref } = useProject();
  return (
    <ChatView
      input={messagesOf(span.input)}
      output={messagesOf(span.output)}
      systemInstructions={span.system_instructions}
      toolDefinitions={span.tool_definitions}
      projectRef={ref}
    />
  );
}

function messagesOf(payload: Schema<"Payload"> | undefined): Schema<"Message">[] {
  if (payload?.format === "messages") return payload.value;
  return [];
}
