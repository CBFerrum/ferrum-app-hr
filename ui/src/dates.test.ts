import { describe, expect, it } from "vitest";
import { addDays, addMonths, daysBetween, eachDay, endOfMonth, isDay, span, startOfMonth, startOfWeek, weekday, within } from "./dates";

describe("days", () => {
  it("counts from Monday", () => {
    // 1 November 2026 is a Sunday.
    expect([weekday("2026-10-26"), weekday("2026-11-01")]).toEqual([0, 6]);
    expect(startOfWeek("2026-11-01")).toBe("2026-10-26");
    expect(startOfWeek("2026-10-26")).toBe("2026-10-26");
  });

  it("moves across months and years without losing a day", () => {
    expect(addDays("2026-02-27", 2)).toBe("2026-03-01");
    expect(addDays("2026-12-31", 1)).toBe("2027-01-01");
    expect(addDays("2026-03-01", -1)).toBe("2026-02-28");
    expect(addMonths("2026-01-31", 1)).toBe("2026-02-28");
    expect(addMonths("2026-03-15", -3)).toBe("2025-12-15");
    expect(addMonths("2026-11-30", 3)).toBe("2027-02-28");
    expect([startOfMonth("2026-10-17"), endOfMonth("2026-10-17"), endOfMonth("2028-02-10")]).toEqual(["2026-10-01", "2026-10-31", "2028-02-29"]);
  });

  it("counts and lists days", () => {
    expect(daysBetween("2026-10-12", "2026-10-14")).toBe(2);
    expect(daysBetween("2026-10-14", "2026-10-12")).toBe(-2);
    expect(eachDay("2026-10-30", "2026-11-02")).toEqual(["2026-10-30", "2026-10-31", "2026-11-01", "2026-11-02"]);
    expect(eachDay("2026-11-02", "2026-10-30")).toEqual([]);
    expect([within("2026-10-13", "2026-10-12", "2026-10-14"), within("2026-10-15", "2026-10-12", "2026-10-14")]).toEqual([true, false]);
    expect([isDay("2026-10-13"), isDay("13/10/2026"), isDay(null)]).toEqual([true, false, false]);
  });

  it("says a span the short way", () => {
    expect(span("2026-10-12", "2026-10-12", "en-GB")).toBe("12 Oct");
    expect(span("2026-10-12", null, "en-GB")).toBe("12 Oct");
    // The dash and its spaces are the language's own.
    expect(span("2026-10-12", "2026-10-14", "en-GB")).toMatch(/^12\s?[–-]\s?14 Oct$/);
    expect(span("2026-10-30", "2026-11-02", "en-GB")).toMatch(/^30 Oct\s?[–-]\s?2 Nov$/);
    expect(span("2026-10-12", "2026-10-14", "en-US")).toMatch(/^Oct 12\s?[–-]\s?14$/);
  });
});
