import { describe, expect, it } from "vitest";
import { type Away, awayOn, dayNote, departments, heading, holidayOn, move, part, rows, shown, weeks } from "./calendar";

const away = (employee: string, from: string, to: string, extra: Partial<Away> = {}): Away => ({ employee, name: employee, from_date: from, to_date: to, ...extra });

describe("the team calendar", () => {
  it("draws the whole weeks that hold a month, starting on Monday", () => {
    // October 2026 starts on a Thursday and ends on a Saturday.
    expect(shown("month", "2026-10-17")).toEqual({ from: "2026-09-28", to: "2026-11-01" });
    const grid = weeks("month", "2026-10-17");
    expect(grid.length).toBe(5);
    expect(grid[0][0]).toBe("2026-09-28");
    expect(grid[4][6]).toBe("2026-11-01");
    // A month that starts on a Monday needs no days before it.
    expect(shown("month", "2026-06-10")).toEqual({ from: "2026-06-01", to: "2026-07-05" });
    expect(shown("week", "2026-10-14")).toEqual({ from: "2026-10-12", to: "2026-10-18" });
  });

  it("steps by the week or the month", () => {
    expect(move("week", "2026-10-14", 1)).toBe("2026-10-21");
    expect(move("week", "2026-10-14", -1)).toBe("2026-10-07");
    expect(move("month", "2026-10-31", 1)).toBe("2026-11-01");
    expect(move("month", "2026-01-15", -1)).toBe("2025-12-01");
  });

  it("names what it shows", () => {
    expect(heading("month", "2026-10-17", "en-GB")).toBe("October 2026");
    expect(heading("week", "2026-10-14", "en-GB")).toMatch(/^12\s?[–-]\s?18 Oct 2026$/);
    expect(heading("week", "2026-10-29", "en-GB")).toMatch(/^26 Oct\s?[–-]\s?1 Nov 2026$/);
  });

  it("knows who is away on a day, decided leave first", () => {
    const list = [away("Zoe", "2026-10-12", "2026-10-14"), away("Asha", "2026-10-13", "2026-10-13", { pending: true }), away("Ben", "2026-10-13", "2026-10-15")];
    expect(awayOn(list, "2026-10-13").map((a) => a.employee)).toEqual(["Ben", "Zoe", "Asha"]);
    expect(awayOn(list, "2026-10-16")).toEqual([]);
    expect([part(list[0], "2026-10-12"), part(list[1], "2026-10-13"), part(away("X", "2026-10-12", "2026-10-12", { half_day_date: "2026-10-12" }), "2026-10-12")]).toEqual(["full", "pending", "half"]);
  });

  it("gives a person one row for the week, however many stretches of leave they have", () => {
    const days = ["2026-10-12", "2026-10-13", "2026-10-14", "2026-10-15", "2026-10-16", "2026-10-17", "2026-10-18"];
    const list = [
      away("b", "2026-10-13", "2026-10-13", { name: "Ben" }),
      away("a", "2026-10-09", "2026-10-12", { name: "Asha", leave_type: "Casual Leave" }),
      away("a", "2026-10-16", "2026-10-16", { name: "Asha", half_day_date: "2026-10-16" }),
      away("a", "2026-10-12", "2026-10-12", { name: "Asha", pending: true }),
      away("c", "2026-10-26", "2026-10-27", { name: "Cy" }),
    ];
    const grid = rows(list, days);
    expect(grid.map((r) => r.name)).toEqual(["Asha", "Ben"]);
    expect(grid[0].cells).toEqual(["full", null, null, null, "half", null, null]);
    expect(grid[0].leave_type).toBe("Casual Leave");
    expect(grid[1].cells).toEqual([null, "full", null, null, null, null, null]);
  });

  it("says what a day holds in words", () => {
    const holidays = [
      { date: "2026-10-02", name: "Gandhi Jayanti", weekly_off: false },
      { date: "2026-10-03", name: "Saturday", weekly_off: true },
    ];
    const list = [away("a", "2026-10-02", "2026-10-02"), away("b", "2026-10-02", "2026-10-02"), away("a", "2026-10-02", "2026-10-02")];
    expect(dayNote(list, holidayOn(holidays, "2026-10-02"), "2026-10-02")).toBe("holiday: Gandhi Jayanti, 2 away");
    expect(dayNote([], holidayOn(holidays, "2026-10-03"), "2026-10-03")).toBe("weekly off");
    expect(dayNote([], undefined, "2026-10-05")).toBe("");
    expect(departments([away("a", "2026-10-02", "2026-10-02", { department: "Support" }), away("b", "2026-10-02", "2026-10-02", { department: "Finance" }), away("c", "2026-10-02", "2026-10-02")])).toEqual([
      "Finance",
      "Support",
    ]);
  });
});
