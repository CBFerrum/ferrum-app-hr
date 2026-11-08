// Approvals: what the server lists as waiting for a manager, and the words a card says about each.
// Pure, so they are tested without a browser.

import { span } from "./dates";
import { days } from "./people";
import { t as tr } from "@ferrum/client";

export type Kind = "leave" | "attendance" | "expense";

export interface Who {
  id: string | null;
  name: string | null;
  department?: string | null;
}

interface Base {
  id: string;
  version: number;
  employee: Who;
  /** What the employee wrote: why they need the leave, what happened, what the claim is for. */
  reason: string | null;
  waiting_since: string | null;
}

export interface Team {
  label: string;
  /** People in it, the one asking included. */
  size: number;
  away: { id: string; name: string | null; from_date: string; to_date: string }[];
  away_count: number;
  asking_count: number;
}

export interface LeaveItem extends Base {
  kind: "leave";
  entity: "LeaveApplication";
  summary: { leave_type: string | null; from_date: string; to_date: string; half_day: boolean; days: string | number | null };
  context: { balance_before: string | null; balance_after: string | null; team: Team | null } | null;
}

export interface AttendanceItem extends Base {
  kind: "attendance";
  entity: "AttendanceRequest";
  summary: { reason: string | null; from_date: string; to_date: string; half_day: boolean; in_time: string | null; out_time: string | null };
  context: { recorded: { date: string; status: string }[]; punches: { at: string; log_type: string | null }[] } | null;
}

export interface Line {
  id: string;
  date: string | null;
  type: string | null;
  description: string | null;
  amount: string | null;
  sanctioned: string | null;
}

export interface ExpenseItem extends Base {
  kind: "expense";
  entity: "ExpenseClaim";
  summary: { currency: string; precision: number; claimed: string | null; sanctioned: string | null; taxes: string | null; advance: string | null; total: string | null; lines: Line[] };
  context: null;
}

export type Item = LeaveItem | AttendanceItem | ExpenseItem;

export const KINDS: { id: Kind; label: string }[] = [
  { id: "leave", get label() { return tr("Leave"); } },
  { id: "attendance", get label() { return tr("Attendance"); } },
  { id: "expense", get label() { return tr("Expenses"); } },
];

export const kindLabel = (k: Kind): string => KINDS.find((x) => x.id === k)?.label ?? k;

/** How many of each kind are waiting. */
export function counts(items: Item[]): Record<Kind, number> {
  const n: Record<Kind, number> = { leave: 0, attendance: 0, expense: 0 };
  for (const i of items) n[i.kind]++;
  return n;
}

/** The items of one kind (or all), keeping their order: the oldest first. */
export const only = (items: Item[], kind: Kind | null): Item[] => (kind ? items.filter((i) => i.kind === kind) : items);

/** An amount in its currency, for reading, not for adding up. */
export function money(v: string | number | null | undefined, currency: string, precision = 2): string {
  const n = Number(v ?? 0);
  if (!Number.isFinite(n)) return String(v);
  if (!currency) return n.toLocaleString(undefined, { minimumFractionDigits: precision, maximumFractionDigits: precision });
  try {
    return new Intl.NumberFormat(undefined, { style: "currency", currency, minimumFractionDigits: precision, maximumFractionDigits: precision }).format(n);
  } catch {
    return `${currency} ${n.toFixed(precision)}`;
  }
}

/** What a request is, in a line: "Casual Leave, 12 to 14 Oct, 3 days". */
export function what(i: Item, locale?: string): string {
  switch (i.kind) {
    case "leave": {
      const s = i.summary;
      return [s.leave_type ?? tr("Leave"), span(s.from_date, s.to_date, locale), days(s.days)].join(", ");
    }
    case "attendance": {
      const s = i.summary;
      const reason = s.reason === "Work From Home" ? tr("Working from home") : s.reason === "On Duty" ? tr("On duty away from the office") : tr("Missed check-in");
      return [reason, span(s.from_date, s.to_date, locale), s.half_day ? tr("with a half day") : ""].filter(Boolean).join(", ");
    }
    case "expense": {
      const s = i.summary;
      const n = s.lines.length;
      return (n === 1 ? tr("Expense claim, {amount} in 1 line", { amount: money(s.claimed, s.currency, s.precision) }) : tr("Expense claim, {amount} in {n} lines", { amount: money(s.claimed, s.currency, s.precision), n }));
    }
  }
}

/** Whole days since `since`, by the calendar (null when there is no date to count from). */
function daysSince(since: string | null | undefined, now: Date): number | null {
  const at = since ? new Date(since) : null;
  if (!at || Number.isNaN(at.getTime())) return null;
  const day = (d: Date) => Date.UTC(d.getFullYear(), d.getMonth(), d.getDate());
  return Math.max(0, Math.round((day(now) - day(at)) / 86_400_000));
}

/** "today", "yesterday", "3 days ago", "2 weeks ago": how long a request has waited. */
export function waiting(since: string | null | undefined, now = new Date()): string {
  const n = daysSince(since, now);
  if (n === null) return "";
  if (n === 0) return "today";
  if (n === 1) return "yesterday";
  if (n < 14) return tr("{n} days ago", { n });
  if (n < 60) return tr("{n} weeks ago", { n: Math.round(n / 7) });
  return tr("{n} months ago", { n: Math.round(n / 30) });
}

/** Whether it has waited long enough to be worth saying so in red: three days. */
export const stale = (since: string | null | undefined, now = new Date()): boolean => (daysSince(since, now) ?? 0) >= 3;

/** What the leave leaves in the balance: "7 days left once it is granted", "2 days short". */
export function balanceLine(i: LeaveItem): string {
  const c = i.context;
  if (!c) return "";
  if (c.balance_after == null) return tr("Leave without pay: no balance to draw on.");
  const after = Number(c.balance_after);
  const before = Number(c.balance_before ?? 0);
  if (after < 0) return tr("{value} short: {before} left.", { value: days(Math.abs(after)), before: days(before) });
  return tr("{after} left once it is granted ({before} now).", { after: days(after), before: days(before) });
}

/** Who else is away then: "2 of 9 in Support are off on these days: Omar and Mina." */
export function teamLine(i: LeaveItem): string {
  const t = i.context?.team;
  if (!t) return "";
  const names = t.away.map((a) => (a.name ?? tr("Someone")).split(/\s+/)[0]);
  const unnamed = t.away_count - names.length;
  const more =
    unnamed > 0 ? tr("{names} and {unnamed} more", { names: names.join(", "), unnamed }) : names.length > 1 ? tr("{value} and {value2}", { value: names.slice(0, -1).join(", "), value2: names[names.length - 1] }) : names.join("");
  const others = Math.max(0, t.size - 1);
  const asked = t.asking_count ? (t.asking_count === 1 ? tr(" One more has asked for them.") : tr(" {count} more have asked for them.", { count: t.asking_count })) : "";
  if (!t.away_count) return (others ? (others === 1 ? tr("Nobody else in {team} is off on these days (1 other).", { team: t.label }) : tr("Nobody else in {team} is off on these days ({others} others).", { team: t.label, others })) : tr("Nobody else in {team} is off on these days.", { team: t.label })) + asked;
  return (t.away_count === 1 ? tr("1 of {size} in {team} is off on these days: {more}.", { size: t.size, team: t.label, more }) : tr("{away} of {size} in {team} are off on these days: {more}.", { away: t.away_count, size: t.size, team: t.label, more })) + asked;
}

/** "Came in 09:05, left 18:10", for what the employee says. */
export function claimedTimes(i: AttendanceItem): string {
  const { in_time, out_time } = i.summary;
  return [in_time ? tr("came in at {in_time}", { in_time }) : "", out_time ? tr("left at {out_time}", { out_time }) : ""].filter(Boolean).join(", ");
}

/** What the days hold now: "Absent on 6 Oct" and the punches there are on them. */
export function recordedLine(i: AttendanceItem, locale?: string): string {
  const c = i.context;
  if (!c) return "";
  const marks = c.recorded.length ? c.recorded.map((r) => tr("{status} on {value}", { status: r.status, value: span(r.date, null, locale) })).join(", ") : tr("Nothing is marked on these days yet");
  const punches = c.punches.length ? c.punches.map((p) => `${p.log_type ? `${p.log_type.toLowerCase()} ` : ""}${p.at.slice(11, 16)}`).join(", ") : "no punches";
  return `${marks}; ${punches}.`;
}

/** The sanctioned total once the approver's changes (line id → text) are applied; unchanged
 *  lines keep what the claim had. For showing, not for the server, which works it out itself. */
export function sanctionedTotal(lines: Line[], edits: Record<string, string>): number {
  return lines.reduce((sum, l) => {
    const v = l.id in edits ? Number(edits[l.id]) : Number(l.sanctioned ?? l.amount ?? 0);
    return sum + (Number.isFinite(v) ? v : 0);
  }, 0);
}

/** The sanctioned amounts that are not valid: not a number, below nothing, or above the claim. */
export function badSanctions(lines: Line[], edits: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const l of lines) {
    if (!(l.id in edits)) continue;
    const v = Number(edits[l.id]);
    if (edits[l.id].trim() === "" || !Number.isFinite(v)) out[l.id] = tr("Enter an amount");
    else if (v < 0) out[l.id] = tr("Cannot be below zero");
    else if (v > Number(l.amount ?? 0)) out[l.id] = tr("Cannot be more than was claimed");
  }
  return out;
}

/** The claim's lines with the approver's sanctioned amounts written in, for sending back whole
 *  (a table is saved as a whole). Only the changed lines differ. */
export function withSanctions<T extends { id?: string; sanctioned_amount?: unknown }>(rows: T[], edits: Record<string, string>): T[] {
  return rows.map((r) => (r.id && r.id in edits ? { ...r, sanctioned_amount: edits[r.id].trim() } : r));
}
