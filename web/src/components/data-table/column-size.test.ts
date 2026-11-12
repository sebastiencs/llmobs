import { expect, it } from "vitest";
import { columnSizesAfterReset, columnSizesAfterResize } from "./column-size";

it("clamps a dragged column at 48 px and rounds", () => {
  expect(columnSizesAfterResize({ name: 200 }, "status", 28.4)).toEqual({
    name: 200,
    status: 48,
  });
  expect(columnSizesAfterResize({}, "name", 240.6)).toEqual({ name: 241 });
});

it("forgets a column size on reset", () => {
  expect(columnSizesAfterReset({ name: 240, start: 132 }, "name")).toEqual({ start: 132 });
  expect(columnSizesAfterReset({ start: 132 }, "name")).toEqual({ start: 132 });
});
