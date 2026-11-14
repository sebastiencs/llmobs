import { expect, it } from "vitest";
import { channelKindLabel } from "./alerts";

it("labels channel kinds", () => {
  expect(channelKindLabel("slack")).toBe("Slack");
  expect(channelKindLabel("webhook")).toBe("Webhook");
  expect(channelKindLabel("email")).toBe("Email");
});
