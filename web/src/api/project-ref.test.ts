import { describe, expect, it } from "vitest";
import { api } from "./client";
import { projectRef } from "./project-ref";

describe("projectRef", () => {
  it("encodes the org/project slash in the request path", async () => {
    let url = "";
    const fetchSpy = (input: Request): Promise<Response> => {
      url = input.url;
      return Promise.resolve(
        new Response("{}", {
          status: 200,
          headers: { "content-type": "application/json" },
        }),
      );
    };

    await api.GET("/api/v1/projects/{project}", {
      params: { path: { project: projectRef("acme", "support-bot") } },
      fetch: fetchSpy,
    });

    expect(url).toContain("/api/v1/projects/acme%2Fsupport-bot");
  });
});
