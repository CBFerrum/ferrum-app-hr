// The team calendar: which days a week or a month shows, who is away on each, and the holidays.
// Pure, so what the grid says is tested without a browser.

import { addDays, addMonths, endOfMonth, monthName, span, startOfMonth, startOfWeek, within } from "./dates";

export type View = "week" | "month";

/** One stretch of leave, as the server tells it (approved, or asked for and not decided yet). */
export interface Away {
  employee: string;
  name: string | null;
  department?: string | null;
  from_date: string;
  to_date: string;
  half_day_date?: string | null;
  pending?: boolean;
  /** Told only to those who read every leave application. */
  leave_type?: string | null;
}

export interface Holiday {
  date: string;
  name: string;
  weekly_off: boolean;
}

/** The days a screen draws: a week, or the whole weeks that hold a month. */
export function shown(view: View, anchor: string): { from: string; to: string } {
  if (view === "week") {
    const from = startOfWeek(anchor);
    return { from, to: addDays(from, 6) };
  }
  return { from: startOfWeek(startOfMonth(anchor)), to: addDays(startOfWeek(endOfMonth(anchor)), 6) };
}

/** The weeks of a month's grid, each of seven days. */
export function weeks(view: View, anchor: string): string[][] {
  const { from, to } = shown(view, anchor);
  const out: string[][] = [];
  for (let d = from; d <= to; d = addDays(d, 7)) out.push(Array.from({ length: 7 }, (_, i) => addDays(d, i)));
  return out;
}

/** The anchor a step earlier or later: a week, or a month. */
export const move = (view: View, anchor: string, step: -1 | 1): string => (view === "week" ? addDays(anchor, 7 * step) : addMonths(startOfMonth(anchor), step));

/** What the screen is called: "October 2026", or "12–18 Oct 2026". */
export function heading(view: View, anchor: string, locale?: string): string {
  if (view === "month") return monthName(anchor, locale);
  const { from, to } = shown("week", anchor);
  return `${span(from, to, locale)} ${to.slice(0, 4)}`;
}

export const covers = (a: Away, day: string): boolean => within(day, a.from_date, a.to_date);

/** How a day of leave counts for someone: all of it, half of it, or asked for and not decided. */
export type Part = "full" | "half" | "pending";

export const part = (a: Away, day: string): Part => (a.pending ? "pending" : a.half_day_date === day ? "half" : "full");

/** Who is away on a day, approved leave before requests, then by name. */
export function awayOn(away: Away[], day: string): Away[] {
  return away
    .filter((a) => covers(a, day))
    .sort((a, b) => Number(!!a.pending) - Number(!!b.pending) || (a.name ?? "").localeCompare(b.name ?? ""));
}

export interface Row {
  employee: string;
  name: string;
  leave_type: string | null;
  /** One entry for each day shown. */
  cells: (Part | null)[];
}

/** One row for each person who is away on some day shown, with how each day counts. */
export function rows(away: Away[], days: string[]): Row[] {
  const by = new Map<string, Row>();
  for (const a of away) {
    if (!days.some((d) => covers(a, d))) continue;
    const row = by.get(a.employee) ?? { employee: a.employee, name: a.name ?? "Someone", leave_type: a.leave_type ?? null, cells: days.map(() => null) };
    days.forEach((d, i) => {
      if (!covers(a, d)) return;
      const next = part(a, d);
      // Decided leave outranks a request for the same day.
      if (row.cells[i] === null || row.cells[i] === "pending") row.cells[i] = next;
    });
    by.set(a.employee, row);
  }
  return [...by.values()].sort((a, b) => a.name.localeCompare(b.name));
}

export const holidayOn = (holidays: Holiday[], day: string): Holiday | undefined => holidays.find((h) => h.date === day);

/** The departments that appear among the people away, for a filter. */
export function departments(away: Away[]): string[] {
  return [...new Set(away.map((a) => a.department).filter((d): d is string => !!d))].sort((a, b) => a.localeCompare(b));
}

/** What a day says to someone who cannot see it: "2 away", "Holiday: Diwali", "Weekly off". */
export function dayNote(away: Away[], holiday: Holiday | undefined, day: string): string {
  const n = new Set(awayOn(away, day).filter((a) => !a.pending).map((a) => a.employee)).size;
  const parts = [holiday ? (holiday.weekly_off ? "weekly off" : `holiday: ${holiday.name}`) : "", n ? `${n} away` : ""].filter(Boolean);
  return parts.join(", ");
}
