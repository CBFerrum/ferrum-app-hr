import { describe, expect, it } from "vitest";
import {
  type AttendanceItem,
  badSanctions,
  balanceLine,
  claimedTimes,
  counts,
  type ExpenseItem,
  type Item,
  type LeaveItem,
  money,
  only,
  recordedLine,
  sanctionedTotal,
  stale,
  teamLine,
  waiting,
  what,
  withSanctions,
} from "./inbox";

const who = { id: "e1", name: "Esha Test" };
const leave = (extra: Partial<LeaveItem> = {}): LeaveItem => ({
  kind: "leave",
  entity: "LeaveApplication",
  id: "l1",
  version: 1,
  employee: who,
  reason: "Family wedding",
  waiting_since: "2026-10-10T08:00:00Z",
  summary: { leave_type: "Casual Leave", from_date: "2026-10-12", to_date: "2026-10-14", half_day: false, days: "3" },
  context: { balance_before: "10", balance_after: "7", team: { label: "Support", size: 9, away: [{ id: "o", name: "Omar Test", from_date: "2026-10-13", to_date: "2026-10-13" }], away_count: 1, asking_count: 0 } },
  ...extra,
});

describe("approvals", () => {
  it("tells a request in a line", () => {
    expect(what(leave(), "en-GB")).toMatch(/^Casual Leave, 12\s?[–-]\s?14 Oct, 3 days$/);
    const att: AttendanceItem = {
      kind: "attendance",
      entity: "AttendanceRequest",
      id: "a1",
      version: 1,
      employee: who,
      reason: "Left without punching out",
      waiting_since: null,
      summary: { reason: "Work From Home", from_date: "2026-10-12", to_date: "2026-10-16", half_day: false, in_time: null, out_time: null },
      context: null,
    };
    expect(what(att, "en-GB")).toMatch(/^Working from home, 12\s?[–-]\s?16 Oct$/);
    expect(what({ ...att, summary: { ...att.summary, reason: "Missed Check-in", to_date: "2026-10-12", half_day: true } }, "en-GB")).toBe("Missed check-in, 12 Oct, with a half day");
    const exp: ExpenseItem = {
      kind: "expense",
      entity: "ExpenseClaim",
      id: "x1",
      version: 1,
      employee: who,
      reason: null,
      waiting_since: null,
      summary: { currency: "INR", precision: 2, claimed: "1200", sanctioned: "1200", taxes: "0", advance: "0", total: "1200", lines: [{ id: "r1", date: "2026-10-15", type: "Travel", description: "Cab", amount: "1200", sanctioned: "1200" }] },
      context: null,
    };
    expect(what(exp)).toMatch(/^Expense claim, .*1,200\.00 in 1 line$/);
  });

  it("counts and filters what is waiting", () => {
    const items: Item[] = [leave(), leave({ id: "l2" }), { ...(leave() as unknown as AttendanceItem), kind: "attendance" }];
    expect(counts(items)).toEqual({ leave: 2, attendance: 1, expense: 0 });
    expect(only(items, "leave").map((i) => i.id)).toEqual(["l1", "l2"]);
    expect(only(items, null).length).toBe(3);
  });

  it("says how long a request has waited, and when that is too long", () => {
    const now = new Date(2026, 9, 14, 12, 0);
    expect(waiting(new Date(2026, 9, 14, 7, 0).toISOString(), now)).toBe("today");
    expect(waiting(new Date(2026, 9, 13, 23, 0).toISOString(), now)).toBe("yesterday");
    expect(waiting(new Date(2026, 9, 10, 9, 0).toISOString(), now)).toBe("4 days ago");
    expect(waiting(new Date(2026, 9, 1, 9, 0).toISOString(), now)).toBe("13 days ago");
    expect(waiting(new Date(2026, 8, 20, 9, 0).toISOString(), now)).toBe("3 weeks ago");
    expect(waiting(new Date(2026, 3, 14, 9, 0).toISOString(), now)).toBe("6 months ago");
    expect(waiting(null, now)).toBe("");
    expect([stale(new Date(2026, 9, 12, 9, 0).toISOString(), now), stale(new Date(2026, 9, 11, 9, 0).toISOString(), now), stale(null, now)]).toEqual([false, true, false]);
  });

  it("says what a leave leaves and who else is away", () => {
    expect(balanceLine(leave())).toBe("7 days left once it is granted (10 days now).");
    expect(balanceLine(leave({ context: { balance_before: "2", balance_after: "-1", team: null } }))).toBe("1 day short: 2 days left.");
    expect(balanceLine(leave({ context: { balance_before: null, balance_after: null, team: null } }))).toBe("Leave without pay: no balance to draw on.");
    expect(balanceLine(leave({ context: null }))).toBe("");
    expect(teamLine(leave())).toBe("1 of 9 in Support is off on these days: Omar.");
    const busy = leave().context!.team!;
    const names = (n: number) => Array.from({ length: n }, (_, i) => ({ id: `p${i}`, name: `Person${i} X`, from_date: "2026-10-13", to_date: "2026-10-13" }));
    expect(teamLine(leave({ context: { balance_before: "1", balance_after: "0", team: { ...busy, away: names(2), away_count: 2 } } }))).toBe("2 of 9 in Support are off on these days: Person0 and Person1.");
    expect(teamLine(leave({ context: { balance_before: "1", balance_after: "0", team: { ...busy, away: names(3), away_count: 5, asking_count: 1 } } }))).toBe(
      "5 of 9 in Support are off on these days: Person0, Person1, Person2 and 2 more. One more has asked for them.",
    );
    expect(teamLine(leave({ context: { balance_before: "1", balance_after: "0", team: { ...busy, away: [], away_count: 0 } } }))).toBe("Nobody else in Support is off on these days (8 others).");
    expect(teamLine(leave({ context: { balance_before: "1", balance_after: "0", team: null } }))).toBe("");
  });

  it("says what the days hold and what the employee claims", () => {
    const att: AttendanceItem = {
      kind: "attendance",
      entity: "AttendanceRequest",
      id: "a1",
      version: 1,
      employee: who,
      reason: null,
      waiting_since: null,
      summary: { reason: "Missed Check-in", from_date: "2026-10-06", to_date: "2026-10-06", half_day: false, in_time: "09:05", out_time: "18:10" },
      context: { recorded: [{ date: "2026-10-06", status: "Absent" }], punches: [{ at: "2026-10-06T09:40", log_type: "IN" }] },
    };
    expect(claimedTimes(att)).toBe("came in at 09:05, left at 18:10");
    expect(recordedLine(att, "en-GB")).toBe("Absent on 6 Oct; in 09:40.");
    expect(recordedLine({ ...att, context: { recorded: [], punches: [] } })).toBe("Nothing is marked on these days yet; no punches.");
  });

  it("works out a claim once the approver changes what is sanctioned", () => {
    const lines = [
      { id: "a", date: null, type: "Travel", description: null, amount: "5000", sanctioned: "5000" },
      { id: "b", date: null, type: "Food", description: null, amount: "1500", sanctioned: "1500" },
    ];
    expect(sanctionedTotal(lines, {})).toBe(6500);
    expect(sanctionedTotal(lines, { b: "1200" })).toBe(6200);
    expect(badSanctions(lines, { a: "5000", b: "1500.01" })).toEqual({ b: "Cannot be more than was claimed" });
    expect(badSanctions(lines, { a: "", b: "-1" })).toEqual({ a: "Enter an amount", b: "Cannot be below zero" });
    expect(badSanctions(lines, {})).toEqual({});
    const rows = [{ id: "a", sanctioned_amount: "5000", amount: "5000" }, { id: "b", sanctioned_amount: "1500", amount: "1500" }];
    expect(withSanctions(rows, { b: " 1200 " })).toEqual([rows[0], { id: "b", sanctioned_amount: "1200", amount: "1500" }]);
  });

  it("writes an amount in its currency", () => {
    expect(money("1200", "INR")).toContain("1,200.00");
    expect(money("1200", "")).toBe("1,200.00");
    expect(money("12", "ZZZZ")).toBe("ZZZZ 12.00");
  });
});
