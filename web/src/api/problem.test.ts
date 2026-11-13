import { describe, expect, it } from "vitest";
import { fieldErrors, isProblem, problemMessage } from "./problem";

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

describe("fieldErrors and problemMessage", () => {
  it("keeps the first message per field", () => {
    expect(
      fieldErrors({
        ...problem,
        status: 400,
        code: "bad_request",
        errors: [
          { field: "email", message: "invalid email" },
          { field: "email", message: "second" },
          { field: "password", message: "too short" },
        ],
      }),
    ).toEqual({ email: "invalid email", password: "too short" });
  });

  it("returns no field error without a problem or errors", () => {
    expect(fieldErrors(problem)).toEqual({});
    expect(fieldErrors(new Error("x"))).toEqual({});
  });

  it("prefers the detail, then the title, then the fallback", () => {
    expect(problemMessage(problem, "fallback")).toBe("missing");
    const { detail: _detail, ...withoutDetail } = problem;
    expect(problemMessage(withoutDetail, "fallback")).toBe("Not Found");
    expect(problemMessage(new Error("x"), "fallback")).toBe("fallback");
  });
});
