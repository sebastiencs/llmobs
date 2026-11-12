import { expect, it } from "vitest";
import {
  columnChoiceKey,
  readColumnChoice,
  reorderVisible,
  visibleColumnIds,
  writeColumnChoice,
} from "@/components/data-table/column-layout";

const specs = [
  { id: "status", locked: true, defaultVisible: true },
  { id: "start", defaultVisible: true },
  { id: "name", locked: true, defaultVisible: true },
  { id: "cost", defaultVisible: true },
  { id: "score:helpfulness", defaultVisible: true },
  { id: "env", defaultVisible: true },
  { id: "release" },
];

it("uses defaults, with score columns where the specs put them", () => {
  expect(visibleColumnIds(specs, undefined).map((id) => id)).toEqual([
    "status",
    "start",
    "name",
    "cost",
    "score:helpfulness",
    "env",
  ]);
});

it("restores visibility and order and ignores an unknown id", () => {
  expect(visibleColumnIds(specs, ["nope", "env", "cost", "score:missing"])).toEqual([
    "status",
    "name",
    "env",
    "cost",
  ]);
});

it("reorders a visible id and refuses the ends", () => {
  expect(reorderVisible(["status", "name", "cost"], "cost", -1)).toEqual([
    "status",
    "cost",
    "name",
  ]);
  expect(reorderVisible(["status", "name"], "status", -1)).toBeUndefined();
});

it("reads and writes the table's column choice", () => {
  expect(readColumnChoice("traces")).toBeUndefined();
  writeColumnChoice("traces", ["env", "name"]);
  expect(localStorage.getItem(columnChoiceKey("traces"))).toBe("env,name");
  expect(readColumnChoice("traces")).toEqual(["env", "name"]);
  writeColumnChoice("traces", undefined);
  expect(localStorage.getItem(columnChoiceKey("traces"))).toBeNull();
});
