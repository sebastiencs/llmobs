import { describe, expect, it } from "vitest";
import { isProblem } from "./problem";

const problem = {
  type: "about:blank",
  title: "Not Found",
  status: 404,
  code: "not_found",
  detail: "missing",
};

describe("isProblem", () => {
  it("accepts a problem with and without detail", () => {
    expect(isProblem(problem)).toBe(true);
    const { detail: _detail, ...withoutDetail } = problem;
    expect(isProblem(withoutDetail)).toBe(true);
  });

  it("rejects values that are not problems", () => {
    expect(isProblem(null)).toBe(false);
    expect(isProblem("nope")).toBe(false);
    expect(isProblem({ title: "Not Found", status: 404 })).toBe(false);
    expect(isProblem({ ...problem, status: "404" })).toBe(false);
  });
});
