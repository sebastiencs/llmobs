import type { Schema } from "@/api/types";

const KIND_LABELS: Record<Schema<"ChannelKind">, string> = {
  slack: "Slack",
  webhook: "Webhook",
  email: "Email",
};

/** "Slack", "Webhook" or "Email". */
export function channelKindLabel(kind: Schema<"ChannelKind">): string {
  return KIND_LABELS[kind];
}
