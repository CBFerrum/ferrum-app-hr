// A person's own space: their leave balances as meters, the month's attendance, what is coming up,
// their requests together, their payslips' periods and what their training covers. Pure, so what
// the screen says is tested without a browser.

import { addDays, daysBetween, endOfMonth, isDay, span, startOfMonth, startOfWeek, within } from "./dates";
import type { Holiday } from "./calendar";

// ───────────── balances ─────────────

/** A row of the leave balance report (decimals arrive as text). */
export interface BalanceRow {
  leave_type: string;
  allocated: string;
  /** Taken and booked (approved, for days to come). */
  taken: string;
  booked: string;
  /** Asked for and not decided. */
  pending: string;
  /** What is left before the days booked and asked for. */
  balance: string;
  /** What can still be asked for. */
  available: string;
}

const n = (v: string | number | null | undefined): number => {
  const x = Number(v ?? 0);
  return Number.isFinite(x) ? x : 0;
};

/** How an allocation divides, as shares of a bar: spent (taken or booked), asked for, and left. */
export function meter(b: BalanceRow): { spent: number; asked: number; left: number } {
  const total = n(b.allocated);
  if (total <= 0) return { spent: 0, asked: 0, left: 0 };
  const share = (v: number) => Math.round(Math.min(100, Math.max(0, (v / total) * 100)) * 10) / 10;
  const asked = share(n(b.pending));
  const left = share(Math.max(0, n(b.available)));
  return { spent: Math.max(0, Math.round((100 - asked - left) * 10) / 10), asked, left };
}

// ───────────── the month's attendance ─────────────

export type Mark = "Present" | "Work From Home" | "Half Day" | "Absent" | "On Leave";

/** The word a mark is drawn with (a colour is never the only thing that says it). */
export const MARKS: Record<Mark, { tone: "present" | "home" | "half" | "absent" | "leave"; letter: string }> = {
  Present: { tone: "present", letter: "P" },
  "Work From Home": { tone: "home", letter: "H" },
  "Half Day": { tone: "half", letter: "½" },
  Absent: { tone: "absent", letter: "A" },
  "On Leave": { tone: "leave", letter: "L" },
};

export interface Attended {
  attendance_date: string;
  status: Mark | string;
  in_time?: string | null;
  out_time?: string | null;
  working_hours?: string | number | null;
  late_entry?: boolean | null;
  early_exit?: boolean | null;
}

export interface MonthDay {
  day: string;
  inMonth: boolean;
  mark: Mark | null;
  /** A holiday's name, or "Weekly off". */
  off: string | null;
  future: boolean;
  today: boolean;
}

/** The weeks of a month (Monday first), each day with its mark, whether it is an off day, and
 *  whether it is still to come. A day with no mark and not off is simply blank. */
export function attendanceMonth(anchor: string, attended: Attended[], holidays: Holiday[], today: string): MonthDay[][] {
  const marks = new Map(attended.map((a) => [a.attendance_date, a.status as Mark]));
  const off = new Map(holidays.map((h) => [h.date, h.weekly_off ? "Weekly off" : h.name]));
  const weeks: MonthDay[][] = [];
  const first = startOfMonth(anchor);
  const last = endOfMonth(anchor);
  for (let w = startOfWeek(first); w <= last; w = addDays(w, 7)) {
    weeks.push(
      Array.from({ length: 7 }, (_, i) => {
        const day = addDays(w, i);
        return { day, inMonth: within(day, first, last), mark: marks.get(day) ?? null, off: off.get(day) ?? null, future: day > today, today: day === today };
      }),
    );
  }
  return weeks;
}

/** The month in numbers: days of each kind, the late entries and early exits, and the hours. */
export function monthCounts(attended: Attended[]): { present: number; home: number; half: number; absent: number; leave: number; late: number; early: number; hours: number } {
  const c = { present: 0, home: 0, half: 0, absent: 0, leave: 0, late: 0, early: 0, hours: 0 };
  for (const a of attended) {
    if (a.status === "Present") c.present++;
    else if (a.status === "Work From Home") c.home++;
    else if (a.status === "Half Day") c.half++;
    else if (a.status === "Absent") c.absent++;
    else if (a.status === "On Leave") c.leave++;
    if (a.late_entry) c.late++;
    if (a.early_exit) c.early++;
    c.hours += n(a.working_hours);
  }
  c.hours = Math.round(c.hours * 100) / 100;
  return c;
}

/** Days that may well be a mistake worth putting right: marked absent, or half a day. */
export const fixable = (a: { status: string }): boolean => a.status === "Absent" || a.status === "Half Day";

// ───────────── what is coming up ─────────────

export interface Time {
  id: string;
  leave_type?: string | null;
  from_date: string;
  to_date: string;
  status: string;
}

export interface Upcoming {
  key: string;
  from: string;
  to: string;
  kind: "leave" | "holiday";
  label: string;
  /** "waiting for approval", for leave not yet decided. */
  note: string;
}

/** The next time off, leave and holidays together, soonest first. Weekly offs are not news. */
export function comingUp(leave: Time[], holidays: Holiday[], today: string, limit = 5): Upcoming[] {
  const items: Upcoming[] = [];
  for (const l of leave) {
    if (!isDay(l.from_date) || l.to_date < today || !["Approved", "Open"].includes(l.status)) continue;
    items.push({ key: `l-${l.id}`, from: l.from_date, to: l.to_date, kind: "leave", label: l.leave_type ?? "Leave", note: l.status === "Open" ? "waiting for approval" : "" });
  }
  for (const h of holidays) {
    if (h.weekly_off || h.date < today) continue;
    items.push({ key: `h-${h.date}`, from: h.date, to: h.date, kind: "holiday", label: h.name, note: "" });
  }
  return items.sort((a, b) => a.from.localeCompare(b.from) || a.kind.localeCompare(b.kind)).slice(0, limit);
}

/** "in 3 days", "tomorrow", "today", "now" (while it lasts). */
export function when(u: { from: string; to: string }, today: string): string {
  if (u.from <= today && today <= u.to) return "now";
  const d = daysBetween(today, u.from);
  return d === 1 ? "tomorrow" : d < 14 ? `in ${d} days` : d < 60 ? `in ${Math.round(d / 7)} weeks` : `in ${Math.round(d / 30)} months`;
}

// ───────────── requests together ─────────────

export interface Request {
  key: string;
  entity: "LeaveApplication" | "AttendanceRequest" | "ExpenseClaim";
  id: string;
  kind: "Leave" | "Attendance" | "Expenses";
  title: string;
  /** The day it is about, to put the latest first. */
  date: string;
  status: string;
}

const ASKS: Record<string, string> = { "Missed Check-in": "Missed check-in", "Work From Home": "Working from home", "On Duty": "On duty" };

/** A claim's status in the words of the person who made it. */
export const claimStatus = (status: string | null | undefined): string => (status === "Draft" ? "Waiting for approval" : status === "Unpaid" ? "Approved, to be paid" : (status ?? ""));

export function requests(
  leave: { id: string; leave_type?: string | null; from_date: string; to_date: string; status: string }[],
  attendance: { id: string; reason?: string | null; from_date: string; to_date: string; status: string }[],
  claims: { id: string; posting_date: string; total_claimed_amount?: string | null; status: string; currency?: string }[],
  limit = 8,
  locale?: string,
): Request[] {
  const all: Request[] = [
    ...leave.map((l) => ({ key: `l-${l.id}`, entity: "LeaveApplication" as const, id: l.id, kind: "Leave" as const, title: `${l.leave_type ?? "Leave"}, ${span(l.from_date, l.to_date, locale)}`, date: l.from_date, status: l.status === "Open" ? "Waiting for approval" : l.status })),
    ...attendance.map((a) => ({
      key: `a-${a.id}`,
      entity: "AttendanceRequest" as const,
      id: a.id,
      kind: "Attendance" as const,
      title: `${ASKS[a.reason ?? ""] ?? "Attendance"}, ${span(a.from_date, a.to_date, locale)}`,
      date: a.from_date,
      status: a.status === "Open" ? "Waiting for approval" : a.status,
    })),
    ...claims.map((c) => ({
      key: `x-${c.id}`,
      entity: "ExpenseClaim" as const,
      id: c.id,
      kind: "Expenses" as const,
      title: `Expense claim${c.total_claimed_amount ? `, ${c.currency ? `${c.currency} ` : ""}${c.total_claimed_amount}` : ""}`,
      date: c.posting_date,
      status: claimStatus(c.status),
    })),
  ];
  return all.sort((a, b) => b.date.localeCompare(a.date)).slice(0, limit);
}

// ───────────── payslips and training ─────────────

/** "October 2026" for a whole month's slip; else the days it covers. */
export function slipPeriod(start: string, end: string, locale?: string): string {
  if (start === startOfMonth(start) && end === endOfMonth(start)) {
    const [y, m] = start.split("-").map(Number);
    return new Date(y, m - 1, 1).toLocaleDateString(locale, { month: "long", year: "numeric" });
  }
  return `${span(start, end, locale)} ${end.slice(0, 4)}`;
}

export type Cover = "valid" | "soon" | "expired";

/** What a training record covers: forever (no end), until a day, soon to lapse (two months), or lapsed. */
export function cover(validUntil: string | null | undefined, today: string): Cover {
  if (!validUntil) return "valid";
  if (validUntil < today) return "expired";
  return daysBetween(today, validUntil) <= 60 ? "soon" : "valid";
}
