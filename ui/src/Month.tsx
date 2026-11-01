// The month's attendance, one disc to a day: present, working from home, half day, absent, on
// leave, and the days off. A day opens to what it holds (when they came in and left, the hours, the
// flags) and to the way to ask for it to be put right, which is what a missed check-in needs
// before payroll runs.

import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useSearchParams } from "react-router";
import { api, todayIso } from "@ferrum/client";
import { Button, Dialog, ErrorState, Icon, Spinner } from "@ferrum/kit";
import { useMe } from "@ferrum/records";
import type { Holiday } from "./calendar";
import { f, rows } from "./data";
import { addDays, addMonths, endOfMonth, monthName, startOfMonth, startOfWeek, weekdayName } from "./dates";
import type { Employee } from "./people";
import { type Attended, attendanceMonth, type Mark, MARKS, type MonthDay, monthCounts } from "./space";

const LONG = (day: string) => {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, { weekday: "long", day: "numeric", month: "long" });
};

const clockTime = (iso: string | null | undefined, zone?: string) =>
  iso ? new Date(iso).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", timeZone: zone }) : "";

function dayWords(d: MonthDay): string {
  const what = d.mark ?? (d.off ? d.off : d.future ? "" : "nothing marked");
  return [LONG(d.day), what].filter(Boolean).join(": ");
}

function DayDialog({ day, mark, zone, onClose, onFix }: { day: MonthDay; mark: Attended | undefined; zone?: string; onClose: () => void; onFix: () => void }) {
  return (
    <Dialog
      open
      onClose={onClose}
      title={LONG(day.day)}
      footer={
        <>
          <Button onClick={onClose}>Close</Button>
          <Button
            variant="primary"
            onClick={() => {
              onClose();
              onFix();
            }}
          >
            Ask for it to be put right
          </Button>
        </>
      }
    >
      {mark ? (
        <dl className="ppl-facts">
          <dt>Marked</dt>
          <dd>{mark.status}</dd>
          {mark.in_time && (
            <>
              <dt>Came in</dt>
              <dd>
                {clockTime(mark.in_time, zone)}
                {mark.late_entry && <span className="ppl-late"> · late</span>}
              </dd>
            </>
          )}
          {mark.out_time && (
            <>
              <dt>Left</dt>
              <dd>
                {clockTime(mark.out_time, zone)}
                {mark.early_exit && <span className="ppl-late"> · early</span>}
              </dd>
            </>
          )}
          {Number(mark.working_hours) > 0 && (
            <>
              <dt>Hours</dt>
              <dd>{Number(mark.working_hours)}</dd>
            </>
          )}
        </dl>
      ) : (
        <p className="muted">Nothing is marked for this day. If you worked, ask for it to be put right, and your approver decides.</p>
      )}
    </Dialog>
  );
}

export function Month({ employee, onFix }: { employee: Employee; onFix: (day: string) => void }) {
  const today = todayIso();
  const zone = useMe().data?.timezone;
  const [sp, setSp] = useSearchParams();
  const asked = sp.get("month");
  const anchor = startOfMonth(asked && /^\d{4}-\d{2}(-\d{2})?$/.test(asked) ? `${asked.slice(0, 7)}-01` : today);
  const from = startOfWeek(anchor);
  const to = addDays(startOfWeek(endOfMonth(anchor)), 6);
  const [open, setOpen] = useState<MonthDay | null>(null);

  const q = useQuery({
    queryKey: ["people", "month", employee.id, anchor],
    queryFn: async ({ signal }) => {
      const [att, cal] = await Promise.all([
        rows<Attended>(
          "Attendance",
          {
            filters: f([["employee", "=", employee.id], ["attendance_date", ">=", from], ["attendance_date", "<=", to], ["docstatus", "=", 1]]),
            sort: "attendance_date asc",
            limit: 100,
            fields: "attendance_date,status,in_time,out_time,working_hours,late_entry,early_exit",
          },
          signal,
        ),
        api.get<{ holidays: Holiday[] }>("/api/app/hr/calendar", { from, to }, signal).catch(() => ({ holidays: [] as Holiday[] })),
      ]);
      return { attended: att.rows, holidays: cal.holidays };
    },
  });

  const weeks = q.data ? attendanceMonth(anchor, q.data.attended, q.data.holidays, today) : [];
  const counts = q.data ? monthCounts(q.data.attended.filter((a) => a.attendance_date.slice(0, 7) === anchor.slice(0, 7))) : null;
  const go = (n: number) => setSp({ month: addMonths(anchor, n).slice(0, 7) }, { replace: true });
  const heads = weeks[0]?.map((d) => d.day) ?? [];
  return (
    <section className="card" aria-labelledby="ppl-month-title">
      <div className="card-head">
        <h2 id="ppl-month-title">Your attendance</h2>
        <div className="ppl-month-nav">
          <button type="button" className="icon-btn" aria-label="Earlier month" onClick={() => go(-1)}>
            <Icon name="left" size={15} />
          </button>
          <span className="ppl-month-name" aria-live="polite">
            {monthName(anchor)}
          </span>
          <button type="button" className="icon-btn" aria-label="Later month" onClick={() => go(1)}>
            <Icon name="chevronRight" size={15} />
          </button>
        </div>
      </div>
      <div className="card-body ppl-month-body">
        {q.error ? (
          <ErrorState error={q.error} onRetry={() => void q.refetch()} />
        ) : !q.data ? (
          <Spinner label="Loading the month" />
        ) : (
          <>
            <table className="ppl-month">
              <thead>
                <tr>
                  {heads.map((d) => (
                    <th key={d} scope="col" abbr={weekdayName(d)}>
                      {weekdayName(d, undefined, "narrow")}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {weeks.map((w) => (
                  <tr key={w[0].day}>
                    {w.map((d) => {
                      const tone = d.mark ? MARKS[d.mark].tone : d.off ? "off" : "none";
                      // A day can be put right once it has happened: marked, or a working day with no mark.
                      const live = d.inMonth && !d.future && (!!d.mark || !d.off);
                      return (
                        <td key={d.day} data-out={!d.inMonth || undefined}>
                          {d.inMonth &&
                            (live ? (
                              <button type="button" className="ppl-day" data-tone={tone} data-today={d.today || undefined} aria-label={dayWords(d)} title={d.off ?? undefined} onClick={() => setOpen(d)}>
                                <span className="ppl-day-n">{Number(d.day.slice(8))}</span>
                                <span className="ppl-day-mark" aria-hidden="true">
                                  {d.mark ? MARKS[d.mark as Mark].letter : d.future ? "" : "·"}
                                </span>
                              </button>
                            ) : (
                              <span className="ppl-day" data-tone={tone} data-today={d.today || undefined} role="img" aria-label={dayWords(d)} title={d.off ?? undefined}>
                                <span className="ppl-day-n">{Number(d.day.slice(8))}</span>
                              </span>
                            ))}
                        </td>
                      );
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
            <ul className="ppl-legend" data-ferrum-nocard>
              {(Object.keys(MARKS) as Mark[]).map((m) => (
                <li key={m} data-tone={MARKS[m].tone}>
                  <span aria-hidden="true">{MARKS[m].letter}</span> {m}
                </li>
              ))}
              <li data-tone="off">
                <span aria-hidden="true" /> Day off
              </li>
            </ul>
            {counts && (counts.present || counts.home || counts.half || counts.absent || counts.leave) ? (
              <p className="faint ppl-counts">
                {[
                  counts.present && `${counts.present} present`,
                  counts.home && `${counts.home} at home`,
                  counts.half && `${counts.half} half ${counts.half === 1 ? "day" : "days"}`,
                  counts.absent && `${counts.absent} absent`,
                  counts.leave && `${counts.leave} on leave`,
                  counts.late && `${counts.late} late`,
                  counts.early && `${counts.early} left early`,
                  counts.hours > 0 && `${counts.hours} hours`,
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </p>
            ) : (
              <p className="muted ppl-counts">Nothing is marked this month yet.</p>
            )}
          </>
        )}
      </div>
      {open && <DayDialog day={open} mark={q.data?.attended.find((a) => a.attendance_date === open.day)} zone={zone} onClose={() => setOpen(null)} onFix={() => onFix(open.day)} />}
    </section>
  );
}
