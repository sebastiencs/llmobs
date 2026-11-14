import { HttpResponse, http } from "msw";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Schema } from "@/api/types";
import {
  defaultExportName,
  downloadSqlExport,
  EXPORT_CUT,
  EXPORT_NOT_SAVED,
  EXPORT_ROLE_REASON,
  exportName,
  exportProblemMessage,
  saveBlob,
} from "@/lib/sql/export";
import { server } from "@/mocks/node";

const BODY: Schema<"SqlQueryRequest"> = {
  query: "SELECT 1",
  from: "2026-01-01T00:00:00Z",
  to: "2026-01-02T00:00:00Z",
};

const PROBLEM_JSON = { "Content-Type": "application/problem+json" };

let restoreDownloads: (() => void) | undefined;
afterEach(() => {
  restoreDownloads?.();
  restoreDownloads = undefined;
});

function captureDownloads() {
  const saved: { href: string; download: string; rel: string }[] = [];
  const revoked: string[] = [];
  const target = URL as unknown as Record<string, unknown>;
  const original = {
    createObjectURL: target.createObjectURL,
    revokeObjectURL: target.revokeObjectURL,
  };
  target.createObjectURL = () => "blob:test";
  target.revokeObjectURL = (url: string) => {
    revoked.push(url);
  };
  const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (
    this: HTMLAnchorElement,
  ) {
    saved.push({ href: this.getAttribute("href") ?? "", download: this.download, rel: this.rel });
  });
  restoreDownloads = () => {
    click.mockRestore();
    for (const [key, value] of Object.entries(original)) {
      if (value === undefined) {
        delete target[key];
      } else {
        target[key] = value;
      }
    }
  };
  return { saved, revoked };
}

describe("defaultExportName", () => {
  it("stamps the UTC instant and uses the format as the extension", () => {
    expect(defaultExportName("csv", new Date("2026-01-01T09:30:05.123Z"))).toBe(
      "query-20260101T093005Z.csv",
    );
    expect(defaultExportName("parquet", new Date("2026-01-01T09:30:05.123Z"))).toMatch(
      /\.parquet$/,
    );
    expect(defaultExportName("ndjson", new Date("2026-01-01T09:30:05.123Z"))).toMatch(/\.ndjson$/);
  });
});

describe("exportName", () => {
  it("reads the Content-Disposition filename", () => {
    expect(exportName('attachment; filename="query-20260101T100000Z.csv"', "f")).toBe(
      "query-20260101T100000Z.csv",
    );
  });

  it("falls back when the disposition is absent or the name unsafe", () => {
    expect(exportName(null, "f")).toBe("f");
    expect(exportName('attachment; filename="../x"', "f")).toBe("f");
    expect(exportName('attachment; filename=".env"', "f")).toBe("f");
  });
});

describe("exportProblemMessage", () => {
  it("covers a missing problem", () => {
    expect(exportProblemMessage(undefined)).toBe("The export could not be downloaded");
  });

  it("explains the member rule for a refusal", () => {
    expect(
      exportProblemMessage({
        type: "about:blank",
        title: "Forbidden",
        status: 403,
        code: "forbidden",
      }),
    ).toBe(EXPORT_ROLE_REASON);
  });

  it("prefers the first field error of an invalid_filter", () => {
    expect(
      exportProblemMessage({
        type: "about:blank",
        title: "Bad Request",
        status: 400,
        code: "invalid_filter",
        errors: [{ field: "query", message: "m" }],
      }),
    ).toBe("m");
  });

  it("prefers the detail otherwise", () => {
    expect(
      exportProblemMessage({
        type: "about:blank",
        title: "Unprocessable Entity",
        status: 422,
        code: "query_timeout",
        detail: "d",
      }),
    ).toBe("d");
  });
});

describe("saveBlob", () => {
  it("downloads through a temporary object URL, revoked on the next task", async () => {
    const { saved, revoked } = captureDownloads();
    saveBlob(new Blob(["x"]), "a.csv");
    expect(saved).toEqual([{ href: "blob:test", download: "a.csv", rel: "noopener" }]);
    expect(document.querySelector("a[download]")).toBeNull();
    expect(revoked).toEqual([]);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(revoked).toEqual(["blob:test"]);
  });
});

describe("downloadSqlExport", () => {
  it("saves the file the server names", async () => {
    const { saved } = captureDownloads();
    const outcome = await downloadSqlExport({
      projectRef: "acme/support-bot",
      body: BODY,
      format: "csv",
    });
    expect(outcome).toEqual({ ok: true, name: "query-20260101T100000Z.csv" });
    expect(saved).toEqual([
      { href: "blob:test", download: "query-20260101T100000Z.csv", rel: "noopener" },
    ]);
  });

  it("falls back to a stamped name without Content-Disposition", async () => {
    captureDownloads();
    server.use(
      http.post(
        "*/api/v1/projects/:project/sql",
        () => new HttpResponse("a\n", { headers: { "Content-Type": "text/csv" } }),
      ),
    );
    const outcome = await downloadSqlExport({
      projectRef: "acme/support-bot",
      body: BODY,
      format: "csv",
    });
    expect(outcome).toEqual({ ok: true, name: expect.stringMatching(/^query-\d{8}T\d{6}Z\.csv$/) });
  });

  it("explains a refusal", async () => {
    const { saved } = captureDownloads();
    server.use(
      http.post("*/api/v1/projects/:project/sql", () =>
        HttpResponse.json(
          { type: "about:blank", title: "Forbidden", status: 403, code: "forbidden" },
          { status: 403, headers: PROBLEM_JSON },
        ),
      ),
    );
    const outcome = await downloadSqlExport({
      projectRef: "acme/support-bot",
      body: BODY,
      format: "csv",
    });
    expect(outcome).toEqual({ ok: false, message: EXPORT_ROLE_REASON });
    expect(saved).toEqual([]);
  });

  it("reports a network failure as a cut export", async () => {
    const { saved } = captureDownloads();
    server.use(http.post("*/api/v1/projects/:project/sql", () => HttpResponse.error()));
    const outcome = await downloadSqlExport({
      projectRef: "acme/support-bot",
      body: BODY,
      format: "csv",
    });
    expect(outcome).toEqual({ ok: false, message: EXPORT_CUT });
    expect(saved).toEqual([]);
  });

  it("reports a body cut after the headers", async () => {
    const { saved } = captureDownloads();
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(new TextEncoder().encode("a,b\r\n"));
            setTimeout(() => controller.error(new Error("cut")), 10);
          },
        }),
        { status: 200, headers: { "Content-Type": "text/csv" } },
      ),
    );
    try {
      const outcome = await downloadSqlExport({
        projectRef: "acme/support-bot",
        body: BODY,
        format: "csv",
      });
      expect(outcome).toEqual({ ok: false, message: EXPORT_CUT });
      expect(saved).toEqual([]);
    } finally {
      fetchSpy.mockRestore();
    }
  });

  it("reports a file that could not be saved", async () => {
    captureDownloads();
    (URL as unknown as Record<string, unknown>).createObjectURL = () => {
      throw new Error("no");
    };
    const outcome = await downloadSqlExport({
      projectRef: "acme/support-bot",
      body: BODY,
      format: "csv",
    });
    expect(outcome).toEqual({ ok: false, message: EXPORT_NOT_SAVED });
  });
});
