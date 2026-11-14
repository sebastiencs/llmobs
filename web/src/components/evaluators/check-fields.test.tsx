import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, it } from "vitest";
import {
  buildCheckConfig,
  CheckFields,
  type CheckName,
  type CheckParams,
  checkStateOf,
  defaultCheckParams,
} from "./check-fields";

/** Controlled harness: the fields read and write their own state, as in the editor. */
function Harness({ check, initial }: { check: CheckName; initial?: CheckParams }) {
  const [value, setValue] = useState<CheckParams>(initial ?? defaultCheckParams(check));
  return (
    <>
      <CheckFields check={check} value={value} onChange={setValue} errors={{}} />
      <output aria-label="state">{JSON.stringify(buildCheckConfig(check, value).config)}</output>
    </>
  );
}

it("shows the controls and defaults of each check", () => {
  const { unmount } = render(<Harness check="exact_match" />);
  expect(screen.getByRole("switch", { name: "Case sensitive" })).toBeChecked();
  expect(screen.getByRole("switch", { name: "Trim whitespace" })).toBeChecked();
  unmount();

  const contains = render(<Harness check="contains" />);
  expect(screen.getByLabelText("Text to find 1")).toHaveValue("");
  expect(screen.getByRole("radio", { name: "Any" })).toBeChecked();
  expect(screen.getByRole("switch", { name: "Case sensitive" })).not.toBeChecked();
  expect(
    screen.getByText("No text: the output is compared with the expected value."),
  ).toBeVisible();
  contains.unmount();

  const regex = render(<Harness check="regex" />);
  expect(screen.getByLabelText("Pattern")).toHaveValue("");
  regex.unmount();

  const schema = render(<Harness check="json_schema" />);
  expect(screen.getByLabelText("JSON Schema")).toHaveValue("{}");
  schema.unmount();

  const levenshtein = render(<Harness check="levenshtein" />);
  expect(
    screen.getByRole("switch", { name: "Normalize (trim, ignore case, collapse spaces)" }),
  ).toBeChecked();
  levenshtein.unmount();
});

it("resets the parameters when another check is picked", () => {
  const first = defaultCheckParams("regex");
  const picked = defaultCheckParams("json_schema");
  expect(first.pattern).toBe("");
  expect(picked.schema).toBe("{}");
  expect(picked.pattern).toBe("");
});

it("drops empty texts, omits values when none is left, and never sends value", async () => {
  const user = userEvent.setup();
  render(<Harness check="contains" />);

  await user.type(screen.getByLabelText("Text to find 1"), "refund");
  await user.click(screen.getByRole("button", { name: "Add text" }));
  await user.click(screen.getByRole("button", { name: "Add text" }));
  expect(screen.getByLabelText("Text to find 3")).toHaveValue("");
  expect(screen.getByLabelText("state")).toHaveTextContent(
    '{"check":"contains","mode":"any","case_sensitive":false,"values":["refund"]}',
  );
  expect(screen.getByLabelText("state").textContent).not.toContain('"value"');

  await user.click(screen.getByRole("button", { name: "Remove text 1" }));
  expect(screen.getByLabelText("state")).toHaveTextContent(
    '{"check":"contains","mode":"any","case_sensitive":false}',
  );
  expect(
    screen.getByText("No text: the output is compared with the expected value."),
  ).toBeVisible();
});

it("round-trips a saved contains config", () => {
  const saved = {
    check: "contains",
    values: ["a", "b"],
    mode: "all",
    case_sensitive: true,
  };
  const { params, check } = checkStateOf(saved);
  expect(check).toBe("contains");
  expect(buildCheckConfig(check, params).config).toEqual(saved);
});

it("refuses an empty pattern and a schema that is not an object", () => {
  expect(buildCheckConfig("regex", defaultCheckParams("regex")).errors).toEqual({
    pattern: "Enter a pattern.",
  });

  const schema = { ...defaultCheckParams("json_schema"), schema: "[1]" };
  expect(buildCheckConfig("json_schema", schema).errors).toEqual({
    schema: "The schema must be a JSON object.",
  });

  const broken = { ...defaultCheckParams("json_schema"), schema: "{" };
  expect(buildCheckConfig("json_schema", broken).errors).toEqual({
    schema: "The schema must be a JSON object.",
  });
});

it("builds the config of each check with only its fields", () => {
  expect(buildCheckConfig("exact_match", defaultCheckParams("exact_match")).config).toEqual({
    check: "exact_match",
    case_sensitive: true,
    trim: true,
  });
  expect(buildCheckConfig("json_valid", defaultCheckParams("json_valid")).config).toEqual({
    check: "json_valid",
  });
  expect(buildCheckConfig("levenshtein", defaultCheckParams("levenshtein")).config).toEqual({
    check: "levenshtein",
    normalize: true,
  });
});
