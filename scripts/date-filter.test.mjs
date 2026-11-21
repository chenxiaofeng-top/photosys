import assert from "node:assert/strict";
import test from "node:test";
import { ALL_DATES, UNKNOWN_DATES, calendarDate, localDateString, monthDateRange, recentDateRange, sameDateFilter, validDate } from "../src/dateFilter.ts";

test("date input validates calendar days and round-trips without a time zone shift", () => {
  for (const invalid of ["2026-2-01", "2026-02-29", "2024-02-30", "2024-04-31", "2024-00-15", "0000-01-01"]) {
    assert.equal(validDate(invalid), false, invalid);
    assert.equal(calendarDate(invalid), undefined, invalid);
  }
  for (const valid of ["2024-02-29", "2000-02-29", "0099-01-01", "2026-11-22"]) {
    assert.equal(localDateString(calendarDate(valid)), valid);
  }
  assert.equal(validDate("1900-02-29"), false);
});

test("month ranges include the last day and compare both ends", () => {
  const february = monthDateRange("2024-02");
  assert.deepEqual(february, { mode: "range", from: "2024-02-01", to: "2024-02-29" });
  assert.equal(monthDateRange("2026-02").to, "2026-02-28");
  assert.equal(sameDateFilter(february, { ...february }), true);
  assert.equal(sameDateFilter(february, { ...february, to: "2024-03-01" }), false);
  assert.equal(sameDateFilter(february, { ...february, to: null }), false);
  assert.equal(sameDateFilter(ALL_DATES, UNKNOWN_DATES), false);
});

test("recent ranges include the local day across a year boundary and daylight saving", () => {
  assert.deepEqual(recentDateRange(new Date(2026, 0, 2, 0, 30), 7), {
    mode: "range", from: "2025-12-27", to: "2026-01-02",
  });
  assert.deepEqual(recentDateRange(new Date(2024, 2, 11, 0, 30), 30), {
    mode: "range", from: "2024-02-11", to: "2024-03-11",
  });
});
