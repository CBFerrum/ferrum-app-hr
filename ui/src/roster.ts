// A roster as the Shift Roster report gives it (people down the side, days across), turned into what the grid
// draws and offers: each cell's shift, what a screen reader hears, and what may be done to it. Pure, so it is
// tested without a browser.

import { t } from "@ferrum/client";

export interface Cell {
  shift_type: string | null;
  assignment: string | null;
  docstatus: 0 | 1 | null;
  start_date: string | null;
  end_date: string | null;
  from_default: boolean;
  holiday: boolean;
  leave: boolean;
}

export interface RosterReport {
  rows: { employee: string; employee_id: string; employee_name: string; department: string | null }[];
  meta: {
    days: string[];
    shift_types: { id: string; name: string; start_time: string; end_time: string }[];
    cells: Record<string, Cell>; // key `${employee_id}:${date}`
    roster: { id: string; title: string; status: "Draft" | "Published"; from_date: string; to_date: string; drafts: number };
  };
}

export type Choice = "set" | "change" | "remove" | "cancel" | "open";

const EMPTY: Cell = { shift_type: null, assignment: null, docstatus: null, start_date: null, end_date: null, from_default: false, holiday: false, leave: false };

/** The cell of a person on a day; an empty one when the report has none. */
export const cellOf = (r: RosterReport, employee: string, day: string): Cell => r.meta.cells[`${employee}:${day}`] ?? EMPTY;

/** Whether the cell's assignment is for that one day alone. */
const oneDay = (c: Cell, day: string): boolean => c.start_date === day && c.end_date === day;

/** What a screen reader hears after the person and the day: the shift and what is special about the day. */
export function cellLabel(c: Cell, shiftName: (id: string) => string): string {
  const parts: string[] = [c.shift_type ? shiftName(c.shift_type) : t("no shift")];
  if (c.assignment && c.docstatus === 0) parts.push(t("draft"));
  if (c.from_default) parts.push(t("from their default shift"));
  if (c.holiday) parts.push(t("holiday"));
  if (c.leave) parts.push(t("on leave"));
  return parts.join(", ");
}

/** What the cell's dialog offers on `day`. */
export function choices(c: Cell, day: string): Choice[] {
  if (!c.assignment) return ["set"];
  if (c.docstatus === 0) return oneDay(c, day) ? ["change", "remove"] : ["open"];
  return oneDay(c, day) ? ["cancel"] : ["open"];
}
