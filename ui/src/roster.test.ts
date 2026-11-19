import { describe, expect, it } from "vitest";
import { type Cell, cellLabel, cellOf, choices, type RosterReport } from "./roster";

const cell = (c: Partial<Cell>): Cell => ({ shift_type: null, assignment: null, docstatus: null, start_date: null, end_date: null, from_default: false, holiday: false, leave: false, ...c });
const name = (id: string) => (id === "day" ? "Day (09:00 to 17:00)" : "Night (21:00 to 05:00)");
const D = "2026-11-26";

describe("a roster's cells", () => {
  it("gives an empty cell for a day the report does not have", () => {
    const r: RosterReport = { rows: [], meta: { days: [D], shift_types: [], cells: {}, roster: { id: "r", title: "Week", status: "Draft", from_date: D, to_date: D, drafts: 0 } } };
    expect(cellOf(r, "e1", D)).toEqual(cell({}));
  });

  it("says what a screen reader should hear", () => {
    expect(cellLabel(cell({ shift_type: "day", assignment: "a1", docstatus: 1, start_date: D, end_date: D }), name)).toBe("Day (09:00 to 17:00)");
    expect(cellLabel(cell({ shift_type: "night", assignment: "a1", docstatus: 0, start_date: D, end_date: D }), name)).toBe("Night (21:00 to 05:00), draft");
    expect(cellLabel(cell({ shift_type: "day", from_default: true }), name)).toBe("Day (09:00 to 17:00), from their default shift");
    expect(cellLabel(cell({ holiday: true }), name)).toBe("no shift, holiday");
    expect(cellLabel(cell({ shift_type: "day", assignment: "a1", docstatus: 1, start_date: D, end_date: D, leave: true }), name)).toBe("Day (09:00 to 17:00), on leave");
  });

  it("offers what may be done to each kind of cell", () => {
    expect(choices(cell({}), D)).toEqual(["set"]);
    expect(choices(cell({ shift_type: "day", from_default: true }), D)).toEqual(["set"]);
    expect(choices(cell({ assignment: "a", docstatus: 0, start_date: D, end_date: D }), D)).toEqual(["change", "remove"]);
    expect(choices(cell({ assignment: "a", docstatus: 0, start_date: D, end_date: "2026-11-30" }), D)).toEqual(["open"]);
    expect(choices(cell({ assignment: "a", docstatus: 1, start_date: D, end_date: D }), D)).toEqual(["cancel"]);
    expect(choices(cell({ assignment: "a", docstatus: 1, start_date: "2026-11-20", end_date: null }), D)).toEqual(["open"]);
  });
});
