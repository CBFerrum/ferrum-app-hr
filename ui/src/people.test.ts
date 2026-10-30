import { describe, expect, it } from "vitest";
import { anniversaries, covers, days, inDaysLabel, localDay, nextLogType } from "./people";

describe("People's home", () => {
  it("knows who is away on a day", () => {
    const l = { id: "a", from_date: "2026-11-03", to_date: "2026-11-04" };
    expect(["2026-11-02", "2026-11-03", "2026-11-04", "2026-11-05"].map((d) => covers(l, d))).toEqual([false, true, true, false]);
    expect(covers({ id: "b", from_date: "2026-11-03" }, "2026-11-03")).toBe(false);
  });

  it("checks in unless the last check-in today was in", () => {
    const now = new Date(2026, 10, 1, 9, 30);
    const today = localDay(now);
    expect(nextLogType(undefined, today)).toBe("IN");
    expect(nextLogType({ log_type: "IN", log_time: now.toISOString() }, today)).toBe("OUT");
    expect(nextLogType({ log_type: "OUT", log_time: now.toISOString() }, today)).toBe("IN");
    expect(nextLogType({ log_type: "IN", log_time: new Date(2026, 9, 31, 18).toISOString() }, today)).toBe("IN");
  });

  it("lists the work anniversaries coming up, not the first year's", () => {
    const e = (id: string, joined: string) => ({ id, employee_name: id, date_of_joining: joined });
    const list = anniversaries([e("five", "2021-11-05"), e("today", "2024-11-01"), e("new", "2026-10-20"), e("later", "2020-12-25"), e("leap", "2024-02-29")], "2026-11-01");
    expect(list.map((a) => [a.employee.id, a.years, a.inDays])).toEqual([
      ["today", 2, 0],
      ["five", 5, 4],
    ]);
    expect(anniversaries([e("leap", "2024-02-29")], "2027-02-20").map((a) => a.date)).toEqual(["2027-02-28"]);
    expect(anniversaries([e("wrap", "2025-01-03")], "2026-12-30").map((a) => [a.date, a.years])).toEqual([["2027-01-03", 2]]);
  });

  it("says days and when", () => {
    expect([days(1), days("2"), days("1.5"), days(null)]).toEqual(["1 day", "2 days", "1.5 days", "0 days"]);
    expect([inDaysLabel(0), inDaysLabel(1), inDaysLabel(5)]).toEqual(["today", "tomorrow", "in 5 days"]);
  });
});
