import { expect, it } from "vitest";
import { parseUserPageSearch, parseUsersSearch } from "./users";

it("keeps valid user search and drops the rest", () => {
  expect(
    parseUsersSearch({
      sort: "cost_total",
      order: "asc",
      eval: "1",
      extra: "nope",
      search: "refund",
    }),
  ).toEqual({
    sort: "cost_total",
    order: "asc",
    eval: 1,
  });
  expect(parseUsersSearch({ sort: "duration" })).toEqual({});
});

it("keeps known column ids and drops the rest", () => {
  expect(parseUsersSearch({ cols: "nope, user, last_seen, user" })).toEqual({
    cols: "user,last_seen",
  });
  expect(parseUsersSearch({ cols: "score:resolved" })).toEqual({});
});

it("keeps the exclusion flags on the user page", () => {
  expect(parseUserPageSearch({ eval: 1, exp: "1", sort: "cost_total" })).toEqual({
    eval: 1,
    exp: 1,
  });
});
