// Days as the screens count them: `YYYY-MM-DD` text, Monday first, worked out in UTC so that no
// time zone or daylight-saving change can move a day. Pure, so it is tested without a browser.

const p2 = (n: number) => String(n).padStart(2, "0");

const parse = (day: string): Date => {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(Date.UTC(y, m - 1, d));
};

const text = (d: Date): string => `${d.getUTCFullYear()}-${p2(d.getUTCMonth() + 1)}-${p2(d.getUTCDate())}`;

export const isDay = (v: unknown): v is string => typeof v === "string" && /^\d{4}-\d{2}-\d{2}$/.test(v);

export const addDays = (day: string, n: number): string => text(new Date(parse(day).getTime() + n * 86_400_000));

/** 0 for Monday to 6 for Sunday. */
export const weekday = (day: string): number => (parse(day).getUTCDay() + 6) % 7;

export const startOfWeek = (day: string): string => addDays(day, -weekday(day));

export const startOfMonth = (day: string): string => `${day.slice(0, 8)}01`;

/** The same day of the month `n` months on, or the last day of that month when it has no such day. */
export function addMonths(day: string, n: number): string {
  const d = parse(day);
  const target = new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + n, 1));
  const last = new Date(Date.UTC(target.getUTCFullYear(), target.getUTCMonth() + 1, 0)).getUTCDate();
  return text(new Date(Date.UTC(target.getUTCFullYear(), target.getUTCMonth(), Math.min(d.getUTCDate(), last))));
}

export const endOfMonth = (day: string): string => addDays(addMonths(startOfMonth(day), 1), -1);

/** Whole days from `a` to `b` (negative when `b` is earlier). */
export const daysBetween = (a: string, b: string): number => Math.round((parse(b).getTime() - parse(a).getTime()) / 86_400_000);

/** Every day from `from` to `to`, both included. */
export function eachDay(from: string, to: string): string[] {
  const out: string[] = [];
  for (let d = from; d <= to && out.length < 400; d = addDays(d, 1)) out.push(d);
  return out;
}

/** Whether `day` falls within `[from, to]`. */
export const within = (day: string, from: string, to: string): boolean => from <= day && day <= to;

/** "12 Oct", for a day. */
export function short(day: string, locale?: string): string {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(locale, { day: "numeric", month: "short" });
}

const at = (day: string): Date => {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d);
};

/** "12 Oct", "12–14 Oct" or "30 Oct – 2 Nov": a stretch of days the way the viewer's language writes it. */
export function span(from: string, to: string | null | undefined, locale?: string): string {
  if (!to || to === from) return short(from, locale);
  return new Intl.DateTimeFormat(locale, { day: "numeric", month: "short" }).formatRange(at(from), at(to));
}

/** "October 2026". */
export function monthName(day: string, locale?: string): string {
  const [y, m] = day.split("-").map(Number);
  return new Date(y, m - 1, 1).toLocaleDateString(locale, { month: "long", year: "numeric" });
}

/** "Mon" to "Sun", for a column head. */
export function weekdayName(day: string, locale?: string, width: "short" | "narrow" = "short"): string {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(locale, { weekday: width });
}
