// People's home, the parts that are plain logic (kept apart, so they are tested): dates as the
// person's own calendar has them, who is away on a day, the work anniversaries coming up, and
// whether the next check-in is in or out.

export interface Employee {
  id: string;
  employee_name?: string | null;
  department?: string | null;
  designation?: string | null;
  date_of_joining?: string | null;
  user?: string | null;
  company_email?: string | null;
  cell_number?: string | null;
  status?: string | null;
  _titles?: Record<string, string>;
}

export interface Leave {
  id: string;
  employee?: string | null;
  employee_name?: string | null;
  leave_type?: string | null;
  from_date?: string | null;
  to_date?: string | null;
  status?: string | null;
  half_day?: boolean | null;
  total_leave_days?: string | number | null;
  _titles?: Record<string, string>;
}

export interface Checkin {
  log_type?: string | null;
  log_time?: string | null;
}

/** A date as YYYY-MM-DD in the browser's time zone. */
export function localDay(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** Whether a leave covers a day (ISO dates compare as text). */
export function covers(l: Leave, day: string): boolean {
  return !!l.from_date && !!l.to_date && l.from_date <= day && day <= l.to_date;
}

/** IN unless the last check-in is an IN of the same day. */
export function nextLogType(last: Checkin | undefined, today: string): "IN" | "OUT" {
  if (!last?.log_time || last.log_type !== "IN") return "IN";
  return localDay(new Date(last.log_time)) === today ? "OUT" : "IN";
}

export interface Anniversary {
  employee: Employee;
  /** The day it falls on, YYYY-MM-DD. */
  date: string;
  years: number;
  inDays: number;
}

const leap = (y: number) => (y % 4 === 0 && y % 100 !== 0) || y % 400 === 0;
const utc = (day: string) => {
  const [y, m, d] = day.split("-").map(Number);
  return { y, m, d, at: Date.UTC(y, m - 1, d) };
};

/** Work anniversaries from `today` to `days` ahead, the soonest first (a 29 February joining is
 *  remembered on the 28th in other years). */
export function anniversaries(employees: Employee[], today: string, days = 14): Anniversary[] {
  const t = utc(today);
  const out: Anniversary[] = [];
  for (const e of employees) {
    if (!e.date_of_joining || !/^\d{4}-\d{2}-\d{2}$/.test(e.date_of_joining)) continue;
    const j = utc(e.date_of_joining);
    for (const year of [t.y, t.y + 1]) {
      const day = j.m === 2 && j.d === 29 && !leap(year) ? 28 : j.d;
      const at = Date.UTC(year, j.m - 1, day);
      const inDays = Math.round((at - t.at) / 86_400_000);
      if (inDays < 0) continue;
      if (inDays <= days && year > j.y) out.push({ employee: e, date: new Date(at).toISOString().slice(0, 10), years: year - j.y, inDays });
      break;
    }
  }
  return out.sort((a, b) => a.inDays - b.inDays || (a.employee.employee_name ?? "").localeCompare(b.employee.employee_name ?? ""));
}

/** "today", "tomorrow", "in 5 days". */
export function inDaysLabel(n: number): string {
  return n === 0 ? "today" : n === 1 ? "tomorrow" : `in ${n} days`;
}

/** Leave days as people say them: "1 day", "2.5 days". */
export function days(n: string | number | null | undefined): string {
  const v = Number(n ?? 0);
  return `${Number.isInteger(v) ? v : v.toFixed(1)} ${v === 1 ? "day" : "days"}`;
}
