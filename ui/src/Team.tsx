// Who is away, by week or month, with the holidays: your own team's, or everyone's for those whose
// rights reach everyone. A month shows how many are away each day and opens one day to the names; a
// week shows each person who is away as a row. Holidays and weekly offs are shaded and named. The
// view, the day and the scope are in the address, so a link shows what you saw.

import { useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router";
import { api, todayIso } from "@ferrum/client";
import { Badge, Button, EmptyState, ErrorState, Icon, Spinner, useShellTitle } from "@ferrum/kit";
import {
  type Away,
  awayOn,
  dayNote,
  departments,
  type Holiday,
  heading,
  holidayOn,
  move,
  part,
  rows as rowsOf,
  shown,
  type View,
  weeks,
} from "./calendar";
import { addDays, isDay, span, weekdayName } from "./dates";
import { Nav } from "./Nav";
import { marks } from "./orgchart";

interface Cal {
  from: string;
  to: string;
  scope: "team" | "all";
  everyone_allowed: boolean;
  size: number;
  has_record: boolean;
  away: Away[];
  holidays: Holiday[];
}

const LONG = (day: string) => {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" });
};

/** The people away on a day, or in a week, as a list a screen reader can read from top to bottom. */
function Names({ away, title }: { away: Away[]; title: string }) {
  if (!away.length) return <p className="muted">Nobody is away.</p>;
  return (
    <section aria-label={title}>
      <ul className="ppl-away" data-ferrum-nocard>
        {away.map((a, i) => (
          <li key={`${a.employee}-${a.from_date}-${i}`}>
            <span className="ppl-avatar" aria-hidden="true">
              {marks(a.name)}
            </span>
            <span className="ppl-away-text">
              <strong>{a.name ?? "Someone"}</strong>
              <span className="faint">
                {[a.leave_type, span(a.from_date, a.to_date), a.half_day_date ? "half a day" : "", a.department].filter(Boolean).join(" · ")}
              </span>
            </span>
            {a.pending && <Badge tone="amber">Asked for</Badge>}
          </li>
        ))}
      </ul>
    </section>
  );
}

function Month({ cal, anchor, today, day, onDay }: { cal: Cal; anchor: string; today: string; day: string; onDay: (d: string) => void }) {
  const grid = weeks("month", anchor);
  const month = anchor.slice(0, 7);
  return (
    <table className="ppl-cal" aria-label={heading("month", anchor)}>
      <thead>
        <tr>
          {grid[0].map((d) => (
            <th key={d} scope="col" abbr={weekdayName(d, undefined, "short")}>
              {weekdayName(d)}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {grid.map((w) => (
          <tr key={w[0]}>
            {w.map((d) => {
              const h = holidayOn(cal.holidays, d);
              const who = awayOn(cal.away, d);
              const decided = who.filter((a) => !a.pending);
              return (
                <td key={d} data-out={d.slice(0, 7) !== month || undefined} data-off={h ? (h.weekly_off ? "week" : "holiday") : undefined}>
                  <button
                    type="button"
                    className="ppl-cal-day"
                    data-today={d === today || undefined}
                    aria-pressed={d === day}
                    aria-label={[LONG(d), dayNote(cal.away, h, d)].filter(Boolean).join(": ")}
                    onClick={() => onDay(d)}
                  >
                    <span className="ppl-cal-n">{Number(d.slice(8))}</span>
                    {h && !h.weekly_off && <span className="ppl-cal-holiday">{h.name}</span>}
                    {who.length > 0 && (
                      <span className="ppl-cal-chips" aria-hidden="true">
                        {decided.slice(0, 3).map((a) => (
                          <span key={a.employee + a.from_date} className="ppl-chip" data-half={part(a, d) === "half" || undefined}>
                            {marks(a.name)}
                          </span>
                        ))}
                        {who.length > Math.min(3, decided.length) && (
                          <span className="ppl-chip" data-more>
                            +{who.length - Math.min(3, decided.length)}
                          </span>
                        )}
                      </span>
                    )}
                    {who.length > 0 && <span className="ppl-cal-count">{who.length}</span>}
                  </button>
                </td>
              );
            })}
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function Week({ cal, anchor, today }: { cal: Cal; anchor: string; today: string }) {
  const { from } = shown("week", anchor);
  const days = Array.from({ length: 7 }, (_, i) => addDays(from, i));
  const people = rowsOf(cal.away, days);
  return (
    <div className="ppl-scroll" tabIndex={0} role="region" aria-label={heading("week", anchor)}>
      <table className="ppl-week">
        <thead>
          <tr>
            <th scope="col" className="ppl-week-name">
              Away
            </th>
            {days.map((d) => {
              const h = holidayOn(cal.holidays, d);
              return (
                <th key={d} scope="col" data-today={d === today || undefined} data-off={h ? (h.weekly_off ? "week" : "holiday") : undefined} title={h?.name}>
                  <span>{weekdayName(d)}</span>
                  <span className="faint">{Number(d.slice(8))}</span>
                </th>
              );
            })}
          </tr>
          {cal.holidays.some((h) => days.includes(h.date) && !h.weekly_off) && (
            <tr>
              <td className="ppl-week-name faint">Holidays</td>
              {days.map((d) => {
                const h = holidayOn(cal.holidays, d);
                return (
                  <td key={d} className="ppl-week-holiday" data-off={h ? (h.weekly_off ? "week" : "holiday") : undefined}>
                    {h && !h.weekly_off ? h.name : ""}
                  </td>
                );
              })}
            </tr>
          )}
        </thead>
        <tbody>
          {people.map((p) => (
            <tr key={p.employee}>
              <th scope="row" className="ppl-week-name">
                <span className="ppl-avatar" aria-hidden="true">
                  {marks(p.name)}
                </span>
                <span>
                  {p.name}
                  {p.leave_type && <span className="faint"> {p.leave_type}</span>}
                </span>
              </th>
              {p.cells.map((c, i) => (
                <td key={days[i]} data-off={holidayOn(cal.holidays, days[i]) ? (holidayOn(cal.holidays, days[i])!.weekly_off ? "week" : "holiday") : undefined}>
                  {c && (
                    <span className="ppl-bar" data-part={c}>
                      <span className="sr-only">{c === "pending" ? "asked for" : c === "half" ? "half a day" : "away"}</span>
                    </span>
                  )}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {!people.length && <p className="muted ppl-week-empty">Everyone is in this week.</p>}
    </div>
  );
}

export function TeamCalendar() {
  useShellTitle("Calendar");
  const today = todayIso();
  const [sp, setSp] = useSearchParams();
  const view: View = sp.get("view") === "week" ? "week" : "month";
  const date = sp.get("date");
  const anchor = date && isDay(date) ? date : today;
  const wantsAll = sp.get("scope") === "all";
  const dept = sp.get("department") ?? "";
  const range = shown(view, anchor);

  const q = useQuery({
    queryKey: ["people", "calendar", range.from, range.to, wantsAll],
    queryFn: ({ signal }) => api.get<Cal>("/api/app/hr/calendar", { from: range.from, to: range.to, scope: wantsAll ? "all" : "team" }, signal),
  });
  const set = (change: Record<string, string | null>) => {
    const next = new URLSearchParams(sp);
    for (const [k, v] of Object.entries(change)) v ? next.set(k, v) : next.delete(k);
    setSp(next, { replace: true });
  };

  const cal = q.data;
  const filtered = cal && dept ? { ...cal, away: cal.away.filter((a) => a.department === dept) } : cal;
  const depts = cal ? departments(cal.away) : [];
  // The day that is open is the one in the address, which is also the month that is shown.
  const day = anchor;

  const body = q.error ? (
    <ErrorState error={q.error} onRetry={() => void q.refetch()} />
  ) : !cal || !filtered ? (
    <Spinner label="Loading the calendar" />
  ) : !cal.has_record && cal.scope === "team" ? (
    <EmptyState title="There is no team to show yet" icon="user">
      <p className="muted">Your sign-in is not linked to an employee record, so there is no team around you. {cal.everyone_allowed ? "You can look at everyone instead." : "HR can link it."}</p>
      {cal.everyone_allowed && <Button onClick={() => set({ scope: "all" })}>Show everyone</Button>}
    </EmptyState>
  ) : (
    <>
      {view === "month" ? <Month cal={filtered} anchor={anchor} today={today} day={day} onDay={(d) => set({ date: d })} /> : <Week cal={filtered} anchor={anchor} today={today} />}
      <ul className="ppl-legend" data-ferrum-nocard>
        <li data-tone="holiday">
          <span aria-hidden="true" /> Holiday
        </li>
        <li data-tone="off">
          <span aria-hidden="true" /> Weekly off
        </li>
        <li data-tone="away">
          <span aria-hidden="true" /> Away
        </li>
        <li data-tone="asked">
          <span aria-hidden="true" /> Asked for, not decided
        </li>
      </ul>
      {view === "month" ? (
        <div className="stack ppl-tight">
          <h2 className="ppl-sub">{LONG(day)}</h2>
          {holidayOn(filtered.holidays, day) && <p className="muted">{holidayOn(filtered.holidays, day)!.weekly_off ? "A weekly off." : `A holiday: ${holidayOn(filtered.holidays, day)!.name}.`}</p>}
          <Names away={awayOn(filtered.away, day)} title={`Away on ${LONG(day)}`} />
        </div>
      ) : (
        <div className="stack ppl-tight">
          <h2 className="ppl-sub">Away this week</h2>
          <Names away={[...filtered.away].sort((a, b) => (a.name ?? "").localeCompare(b.name ?? "") || a.from_date.localeCompare(b.from_date))} title="Away this week" />
        </div>
      )}
    </>
  );

  return (
    <div className="ppl-page ppl-calendar">
      <header className="ppl-head">
        <div>
          <Nav />
          <p className="page-sub" aria-live="polite">
            {cal ? (cal.scope === "all" ? `Everyone, ${cal.size} people` : `Your team, ${cal.size} ${cal.size === 1 ? "person" : "people"}`) : " "}
          </p>
        </div>
      </header>
      <div className="ppl-tools">
        <div className="segmented" role="group" aria-label="Show">
          <button type="button" aria-pressed={view === "month"} onClick={() => set({ view: null })}>
            Month
          </button>
          <button type="button" aria-pressed={view === "week"} onClick={() => set({ view: "week" })}>
            Week
          </button>
        </div>
        <div className="ppl-step">
          <button type="button" className="icon-btn" aria-label={view === "week" ? "Earlier week" : "Earlier month"} onClick={() => set({ date: move(view, anchor, -1) })}>
            <Icon name="left" size={16} />
          </button>
          <h2 className="ppl-cal-title" aria-live="polite">
            {heading(view, anchor)}
          </h2>
          <button type="button" className="icon-btn" aria-label={view === "week" ? "Later week" : "Later month"} onClick={() => set({ date: move(view, anchor, 1) })}>
            <Icon name="chevronRight" size={16} />
          </button>
          <Button size="sm" onClick={() => set({ date: null })} disabled={anchor === today && !date}>
            Today
          </Button>
        </div>
        {cal?.everyone_allowed && (
          <div className="segmented" role="group" aria-label="Whose">
            <button type="button" aria-pressed={!wantsAll} onClick={() => set({ scope: null, department: null })}>
              My team
            </button>
            <button type="button" aria-pressed={wantsAll} onClick={() => set({ scope: "all" })}>
              Everyone
            </button>
          </div>
        )}
        {wantsAll && depts.length > 1 && (
          <select className="select ppl-dept" aria-label="Department" value={dept} onChange={(e) => set({ department: e.target.value || null })}>
            <option value="">All departments</option>
            {depts.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        )}
      </div>
      {body}
    </div>
  );
}
