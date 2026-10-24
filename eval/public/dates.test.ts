import { expect, test } from "bun:test";

import { isoDate } from "./dates";

test("benchmark dates parse to ISO UTC", () => {
  expect(isoDate("1:56 pm on 8 May, 2023")).toBe("2023-05-08T13:56:00.000Z");
  expect(isoDate("12:09 am on 13 September, 2023")).toBe("2023-09-13T00:09:00.000Z");
  expect(isoDate("2023/05/30 (Tue) 23:40")).toBe("2023-05-30T23:40:00.000Z");
  expect(isoDate("7 days 8 hours ago")).toBeUndefined();
  expect(isoDate(undefined)).toBeUndefined();
});
