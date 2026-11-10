import { expectTypeOf, it } from "vitest";
import { api } from "./client";
import type { Schema } from "./types";

it("types the trace id and the signed-in user", () => {
  expectTypeOf<Schema<"TraceSummary">["trace_id"]>().toEqualTypeOf<string>();

  async function loadMe() {
    return api.GET("/api/auth/me");
  }

  type MeData = Awaited<ReturnType<typeof loadMe>>["data"];

  expectTypeOf<NonNullable<MeData>["orgs"]>().toEqualTypeOf<Schema<"OrgMembership">[]>();
});
