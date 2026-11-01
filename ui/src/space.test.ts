import { describe, expect, it } from "vitest";
import { attendanceMonth, claimStatus, comingUp, cover, fixable, meter, monthCounts, requests, slipPeriod, when } from "./space";

describe("a person's own space", () => {
  it("divides an allocation into what is spent, asked for and left", () => {
    const row = { leave_type: "Casual Leave", allocated: "12", taken: "2", booked: "1", pending: "3", balance: "10", available: "6" };
    expect(meter(row)).toEqual({ spent: 25, asked: 25, left: 50 });
    expect(meter({ ...row, allocated: "0" })).toEqual({ spent: 0, asked: 0, left: 0 });
    // Overdrawn: nothing is left, and the bar is full.
    expect(meter({ ...row, allocated: "2", pending: "1", available: "-1" })).toEqual({ spent: 50, asked: 50, left: 0 });
  });

  it("lays a month out in weeks from Monday, with its marks, off days and what is still to come", () => {
    const month = attendanceMonth(
      "2026-10-15",
      [
        { attendance_date: "2026-10-01", status: "Present" },
        { attendance_date: "2026-10-05", status: "Absent" },
      ],
      [
        { date: "2026-10-02", name: "Gandhi Jayanti", weekly_off: false },
        { date: "2026-10-03", name: "Saturday", weekly_off: true },
      ],
      "2026-10-05",
    );
    expect(month.length).toBe(5);
    expect(month[0][0]).toMatchObject({ day: "2026-09-28", inMonth: false });
    const first = month[0];
    expect(first[3]).toMatchObject({ day: "2026-10-01", inMonth: true, mark: "Present", off: null, future: false });
    expect(first[4]).toMatchObject({ day: "2026-10-02", off: "Gandhi Jayanti" });
    expect(first[5]).toMatchObject({ day: "2026-10-03", off: "Weekly off" });
    expect(month[1][0]).toMatchObject({ day: "2026-10-05", mark: "Absent", today: true, future: false });
    expect(month[1][1]).toMatchObject({ day: "2026-10-06", mark: null, future: true });
  });

  it("counts the month", () => {
    const c = monthCounts([
      { attendance_date: "a", status: "Present", late_entry: true, working_hours: "8.5" },
      { attendance_date: "b", status: "Present", early_exit: true, working_hours: 9 },
      { attendance_date: "c", status: "Half Day", working_hours: "4" },
      { attendance_date: "d", status: "Absent" },
      { attendance_date: "e", status: "On Leave" },
      { attendance_date: "f", status: "Work From Home" },
    ]);
    expect(c).toEqual({ present: 2, home: 1, half: 1, absent: 1, leave: 1, late: 1, early: 1, hours: 21.5 });
    expect([fixable({ status: "Absent" }), fixable({ status: "Half Day" }), fixable({ status: "Present" })]).toEqual([true, true, false]);
  });

  it("lists time off and holidays to come, soonest first, leaving out weekly offs and what is past", () => {
    const list = comingUp(
      [
        { id: "1", leave_type: "Casual Leave", from_date: "2026-10-20", to_date: "2026-10-21", status: "Approved" },
        { id: "2", leave_type: "Sick Leave", from_date: "2026-10-01", to_date: "2026-10-02", status: "Approved" },
        { id: "3", leave_type: "Privilege Leave", from_date: "2026-11-02", to_date: "2026-11-06", status: "Open" },
        { id: "4", leave_type: "Casual Leave", from_date: "2026-10-25", to_date: "2026-10-25", status: "Rejected" },
        { id: "5", leave_type: "Casual Leave", from_date: "2026-10-10", to_date: "2026-10-16", status: "Approved" },
      ],
      [
        { date: "2026-10-17", name: "Saturday", weekly_off: true },
        { date: "2026-10-19", name: "Dussehra", weekly_off: false },
        { date: "2026-10-05", name: "Past day", weekly_off: false },
      ],
      "2026-10-15",
    );
    expect(list.map((u) => [u.from, u.label, u.note])).toEqual([
      ["2026-10-10", "Casual Leave", ""],
      ["2026-10-19", "Dussehra", ""],
      ["2026-10-20", "Casual Leave", ""],
      ["2026-11-02", "Privilege Leave", "waiting for approval"],
    ]);
    expect(comingUp([], [], "2026-10-15")).toEqual([]);
    expect(when({ from: "2026-10-10", to: "2026-10-16" }, "2026-10-15")).toBe("now");
    expect([when({ from: "2026-10-16", to: "2026-10-16" }, "2026-10-15"), when({ from: "2026-10-19", to: "2026-10-19" }, "2026-10-15"), when({ from: "2026-11-02", to: "2026-11-02" }, "2026-10-15")]).toEqual([
      "tomorrow",
      "in 4 days",
      "in 3 weeks",
    ]);
  });

  it("puts a person's requests of every kind together, the latest day first", () => {
    const list = requests(
      [{ id: "l", leave_type: "Casual Leave", from_date: "2026-10-12", to_date: "2026-10-14", status: "Open" }],
      [{ id: "a", reason: "Work From Home", from_date: "2026-10-20", to_date: "2026-10-20", status: "Approved" }],
      [{ id: "x", posting_date: "2026-10-16", total_claimed_amount: "1200", status: "Draft", currency: "INR" }],
      8,
      "en-GB",
    );
    expect(list.map((r) => [r.kind, r.title.replace(/\s?[–-]\s?/, " to "), r.status])).toEqual([
      ["Attendance", "Working from home, 20 Oct", "Approved"],
      ["Expenses", "Expense claim, INR 1200", "Waiting for approval"],
      ["Leave", "Casual Leave, 12 to 14 Oct", "Waiting for approval"],
    ]);
    expect(requests([], [], [], 8)).toEqual([]);
    expect(claimStatus("Unpaid")).toBe("Approved, to be paid");
  });

  it("names a payslip's period and what a training covers", () => {
    expect(slipPeriod("2026-09-01", "2026-09-30", "en-GB")).toBe("September 2026");
    expect(slipPeriod("2026-08-15", "2026-08-31", "en-GB")).toMatch(/^15\s?[–-]\s?31 Aug 2026$/);
    expect([cover(null, "2026-10-15"), cover("2026-10-14", "2026-10-15"), cover("2026-11-30", "2026-10-15"), cover("2027-03-31", "2026-10-15")]).toEqual(["valid", "expired", "soon", "valid"]);
  });
});
