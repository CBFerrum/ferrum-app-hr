// Rosters: the list at /roster, and a roster's grid at /roster/{id}: its people down the side and its days across,
// each cell the shift that day. A planner sets or clears a day in a dialog, fills a shift for everyone and publishes;
// everything goes through the operations an agent would use (the Shift Roster report, the shift assignment's record
// operations, the roster's Fill and Publish), and after each the grid is read again. Someone who may only read
// sees the grid without buttons; an employee sees their own row.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { type FormEvent, type KeyboardEvent, useCallback, useRef, useState } from "react";
import { Link, useParams } from "react-router";
import { api, ApiError, docPath, errorMessage, isolate, newKey, t } from "@ferrum/client";
import { Alert, Badge, Button, Dialog, Field, ListSkeleton, StatePanel, TableSkeleton, confirmDialog, toast, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { useMetaIndex } from "@ferrum/records";
import { rows, type Titled } from "./data";
import { longDay, span, weekdayName } from "./dates";
import { Failed, Nav } from "./Nav";
import { type Cell, cellLabel, cellOf, choices, type RosterReport } from "./roster";

interface RosterRow extends Titled {
  id: string;
  title: string;
  from_date: string;
  to_date: string;
  status: "Draft" | "Published";
}

/** What the person may do to an entity: read, write, create (each "none" or more). */
function useRights(entity: string): { read: boolean; write: boolean; create: boolean } | undefined {
  const meta = useMetaIndex().data;
  if (!meta) return undefined;
  const p = meta.entities.find((x) => x.name === entity)?.permissions;
  return { read: !!p && p.read !== "none", write: !!p && p.write !== "none", create: !!p && p.create !== "none" };
}

const statusBadge = (s: "Draft" | "Published") => (s === "Published" ? <Badge tone="green">{t("Published")}</Badge> : <Badge tone="amber">{t("Draft")}</Badge>);

export function Rosters() {
  useShellTitle(t("Rosters"));
  const may = useRights("ShiftRoster");
  const q = useQuery({
    queryKey: ["people", "rosters"],
    queryFn: ({ signal }) => rows<RosterRow>("ShiftRoster", { sort: "from_date desc", limit: 50 }, signal).then((r) => r.rows),
  });
  const create = may?.create ? <Link to="/e/ShiftRoster/new" className="btn btn-primary">{t("New roster")}</Link> : undefined;
  const body = q.error ? (
    <Failed error={q.error} onRetry={() => void q.refetch()} />
  ) : !q.data ? (
    <ListSkeleton />
  ) : !q.data.length ? (
    <StatePanel kind="empty" title={t("No rosters yet")} action={create}>
      {t("A roster plans who works which shift over a range of days.")}
    </StatePanel>
  ) : (
    <ul className="ppl-rosters" data-ferrum-nocard>
      {q.data.map((r) => (
        <li key={r.id}>
          <Link to={`/roster/${r.id}`}>
            <strong>{isolate(r.title)}</strong>
          </Link>
          <span className="faint">{isolate([r._titles?.department, span(r.from_date, r.to_date)].filter(Boolean).join(" · "))}</span>
          {statusBadge(r.status)}
        </li>
      ))}
    </ul>
  );
  return (
    <div className="ppl-page">
      <header className="ppl-head">
        <Nav />
        {q.data && q.data.length > 0 && create}
      </header>
      {body}
    </div>
  );
}

/** The dialog for one person's day: set or change its shift, remove a draft, cancel a one-day shift, or open a longer one. */
function CellDialog({ report, person, day, roster, onClose, onDone }: { report: RosterReport; person: RosterReport["rows"][number]; day: string; roster: string; onClose: () => void; onDone: (said: string) => void }) {
  const c = cellOf(report, person.employee_id, day);
  const offers = choices(c, day);
  const [shift, setShift] = useState(c.shift_type ?? report.meta.shift_types[0]?.id ?? "");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | undefined>();
  const run = async (what: string, call: () => Promise<unknown>, said: string) => {
    setBusy(what);
    setError(null);
    setProblem(undefined);
    try {
      await call();
      onDone(said);
    } catch (err) {
      if (err instanceof ApiError && err.status === 409) setError(t("Someone changed this shift meanwhile"));
      else if (err instanceof ApiError && err.fields.length) {
        setProblem(err.fields.map((f) => f.message).join("; "));
        setError(errorMessage(err));
      } else setError(errorMessage(err));
      setBusy(null);
    }
  };
  const save = (e: FormEvent) => {
    e.preventDefault();
    if (!shift) return setProblem(t("Choose a shift"));
    if (offers.includes("change") && c.assignment) {
      void run("save", () => api.patch(docPath("ShiftAssignment", c.assignment!), { shift_type: shift }), t("Shift changed"));
    } else {
      void run("save", () => api.post("/api/v1/ShiftAssignment", { employee: person.employee_id, shift_type: shift, start_date: day, end_date: day, roster }, { idempotencyKey: newKey() }), t("Shift set"));
    }
  };
  const editable = offers.includes("set") || offers.includes("change");
  return (
    <Dialog
      open
      onClose={onClose}
      title={t("{name}, {day}", { name: person.employee_name, day: longDay(day) })}
      footer={
        <>
          <Button onClick={onClose}>{t("Close")}</Button>
          {offers.includes("remove") && (
            <Button variant="danger" loading={busy === "remove"} disabled={!!busy} onClick={() => void run("remove", () => api.del(docPath("ShiftAssignment", c.assignment!)), t("Shift removed"))}>
              {t("Remove")}
            </Button>
          )}
          {offers.includes("cancel") && (
            <Button variant="danger" loading={busy === "cancel"} disabled={!!busy} onClick={() => void run("cancel", () => api.post(docPath("ShiftAssignment", c.assignment!, "cancel")), t("Shift cancelled"))}>
              {t("Cancel this shift")}
            </Button>
          )}
          {editable && (
            <Button type="submit" form="roster-cell" variant="primary" loading={busy === "save"} disabled={!!busy}>
              {t("Save")}
            </Button>
          )}
        </>
      }
    >
      <form id="roster-cell" className="stack" onSubmit={save} noValidate>
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        <p className="muted">{isolate(cellLabel(c, (id) => shiftLabel(report, id)))}</p>
        {offers.includes("open") && c.assignment && <Link to={`/e/ShiftAssignment/${c.assignment}`}>{t("Part of a longer shift: open it")}</Link>}
        {editable && (
          <Field label={t("Shift")} htmlFor="roster-shift" error={problem}>
            <select id="roster-shift" className="select" value={shift} onChange={(e) => setShift(e.target.value)}>
              {report.meta.shift_types.map((s) => (
                <option key={s.id} value={s.id}>
                  {isolate(t("{name} ({start} to {end})", { name: s.name, start: s.start_time, end: s.end_time }))}
                </option>
              ))}
            </select>
          </Field>
        )}
      </form>
    </Dialog>
  );
}

/** "Day (09:00 to 17:00)", for a shift type of the report. */
function shiftLabel(report: RosterReport, id: string): string {
  const s = report.meta.shift_types.find((x) => x.id === id);
  return s ? t("{name} ({start} to {end})", { name: s.name, start: s.start_time, end: s.end_time }) : t("a shift");
}

function FillDialog({ report, onClose, onDone }: { report: RosterReport; onClose: () => void; onDone: (said: string) => void }) {
  const r = report.meta.roster;
  const [shift, setShift] = useState(report.meta.shift_types[0]?.id ?? "");
  const [from, setFrom] = useState(r.from_date);
  const [to, setTo] = useState(r.to_date);
  const [who, setWho] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [problems, setProblems] = useState<Record<string, string>>({});
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    setProblems({});
    if (!shift) return setProblems({ shift_type: t("Choose a shift") });
    setBusy(true);
    try {
      const out = await api.post<{ created: number; skipped: number }>(
        docPath("ShiftRoster", r.id, "actions", "fill"),
        { shift_type: shift, from_date: from, to_date: to, employee: who || null },
        { idempotencyKey: newKey() },
      );
      onDone(t("{created} people given the shift, {skipped} already had one", { created: out.created, skipped: out.skipped }));
    } catch (err) {
      if (err instanceof ApiError && err.fields.length) setProblems(Object.fromEntries(err.fields.map((x) => [x.field, x.message])));
      setError(errorMessage(err));
      setBusy(false);
    }
  };
  return (
    <Dialog
      open
      onClose={onClose}
      title={t("Fill with a shift")}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button type="submit" form="roster-fill" variant="primary" loading={busy}>
            {t("Fill")}
          </Button>
        </>
      }
    >
      <form id="roster-fill" className="stack" onSubmit={(e) => void submit(e)} noValidate>
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        <Field label={t("Shift")} htmlFor="fill-shift" required error={problems.shift_type}>
          <select id="fill-shift" className="select" value={shift} onChange={(e) => setShift(e.target.value)}>
            {report.meta.shift_types.map((s) => (
              <option key={s.id} value={s.id}>
                {isolate(shiftLabel(report, s.id))}
              </option>
            ))}
          </select>
        </Field>
        <div className="ppl-fix-days">
          <Field label={t("From")} htmlFor="fill-from" error={problems.from_date}>
            <input id="fill-from" className="input" type="date" min={r.from_date} max={r.to_date} value={from} onChange={(e) => setFrom(e.target.value)} />
          </Field>
          <Field label={t("To")} htmlFor="fill-to" error={problems.to_date}>
            <input id="fill-to" className="input" type="date" min={from} max={r.to_date} value={to} onChange={(e) => setTo(e.target.value)} />
          </Field>
        </div>
        <Field label={t("Only for")} htmlFor="fill-who" hint={t("Everyone in the roster when empty")} error={problems.employee}>
          <select id="fill-who" className="select" value={who} onChange={(e) => setWho(e.target.value)}>
            <option value="">{t("Everyone")}</option>
            {report.rows.map((p) => (
              <option key={p.employee_id} value={p.employee_id}>
                {isolate(p.employee_name)}
              </option>
            ))}
          </select>
        </Field>
      </form>
    </Dialog>
  );
}

export function RosterGrid() {
  const { id = "" } = useParams();
  const qc = useQueryClient();
  const key = ["people", "roster", id];
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["people", "roster", id] }), [qc, id]);
  useLiveEntity("ShiftAssignment", refresh);
  const q = useQuery({ queryKey: key, queryFn: ({ signal }) => api.get<RosterReport>("/api/reports/shift_roster", { roster: id }, signal) });
  const roster = useRights("ShiftRoster");
  const shifts = useRights("ShiftAssignment");
  const report = q.data;
  useShellTitle(report?.meta.roster.title ?? t("Roster"));
  const [open, setOpen] = useState<{ person: number; day: number } | null>(null);
  const [filling, setFilling] = useState(false);
  const [publishing, setPublishing] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [focus, setFocus] = useState({ person: 0, day: 0 });
  const grid = useRef<HTMLTableElement>(null);

  const done = (said: string) => {
    setOpen(null);
    setFilling(false);
    toast(said);
    refresh();
  };

  const publish = async () => {
    if (!report) return;
    const ok = await confirmDialog({ title: t("Publish"), message: t("Publish this roster? Its draft shifts start to count and each person is told."), confirmLabel: t("Publish") });
    if (!ok) return;
    setFailure(null);
    const total = report.meta.roster.drafts;
    let published = 0;
    try {
      for (;;) {
        setPublishing(t("Publishing… {done} of {total}", { done: published, total }));
        const out = await api.post<{ published: number; remaining: number }>(docPath("ShiftRoster", id, "actions", "publish"), {}, { idempotencyKey: newKey() });
        published += out.published;
        if (out.remaining === 0) break;
      }
      toast(t("Published: {done} shifts", { done: published }));
    } catch (err) {
      setFailure(t("{error} ({done} shifts were published before this)", { error: errorMessage(err), done: published }));
    }
    setPublishing(null);
    refresh();
  };

  if (q.error instanceof ApiError && q.error.status === 404) {
    return (
      <div className="ppl-page">
        <StatePanel kind="empty" title={t("This roster no longer exists")} action={<Link to="/roster">{t("All rosters")}</Link>} />
      </div>
    );
  }

  const canPlan = !!roster?.write;
  const canEdit = !!shifts?.write;
  const days = report?.meta.days ?? [];
  const people = report?.rows ?? [];

  // One tab stop for the grid; the arrows, Home and End move between its cells, Enter or Space opens one.
  const move = (e: KeyboardEvent<HTMLTableElement>) => {
    const { person, day } = focus;
    const next = { ArrowRight: { person, day: day + 1 }, ArrowLeft: { person, day: day - 1 }, ArrowDown: { person: person + 1, day }, ArrowUp: { person: person - 1, day }, Home: { person, day: 0 }, End: { person, day: days.length - 1 } }[e.key];
    if (!next) return;
    e.preventDefault();
    const to = { person: Math.max(0, Math.min(people.length - 1, next.person)), day: Math.max(0, Math.min(days.length - 1, next.day)) };
    setFocus(to);
    grid.current?.querySelector<HTMLElement>(`[data-cell="${to.person}:${to.day}"]`)?.focus();
  };

  const body = q.error ? (
    <Failed error={q.error} onRetry={() => void q.refetch()} />
  ) : !report ? (
    <TableSkeleton columns={8} rows={6} />
  ) : !people.length ? (
    <StatePanel kind="empty" title={t("Nobody is in this roster")} action={<Link to={`/e/ShiftRoster/${id}`}>{t("Open the roster")}</Link>}>
      {t("Check its department and branch")}
    </StatePanel>
  ) : (
    <div className="ppl-scroll ppl-roster" tabIndex={-1}>
      <table className="ppl-week" ref={grid} onKeyDown={canEdit ? move : undefined}>
        <caption>{isolate(t("{title}, {days}", { title: report.meta.roster.title, days: span(report.meta.roster.from_date, report.meta.roster.to_date) }))}</caption>
        <thead>
          <tr>
            <th scope="col" className="ppl-week-name">
              {t("Person")}
            </th>
            {days.map((d) => (
              <th key={d} scope="col" aria-label={longDay(d)}>
                <span>{isolate(weekdayName(d))}</span> <span className="faint">{Number(d.slice(8))}</span>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {people.map((p, pi) => (
            <tr key={p.employee_id}>
              <th scope="row" className="ppl-week-name">
                {isolate(p.employee_name)}
                {p.department && <span className="faint"> {isolate(p.department)}</span>}
              </th>
              {days.map((d, di) => {
                const c: Cell = cellOf(report, p.employee_id, d);
                const name = c.shift_type ? (report.meta.shift_types.find((s) => s.id === c.shift_type)?.name ?? "") : "";
                const label = t("{name}, {day}: {shift}", { name: p.employee_name, day: longDay(d), shift: cellLabel(c, (sid) => shiftLabel(report, sid)) });
                const text = (
                  <>
                    {isolate(name)}
                    {c.assignment && c.docstatus === 0 && <span className="faint"> {t("draft")}</span>}
                    {c.holiday && <span className="faint"> {t("off")}</span>}
                    {c.leave && <span className="faint"> {t("leave")}</span>}
                  </>
                );
                return (
                  <td key={d} data-off={c.holiday ? "holiday" : undefined} data-draft={c.docstatus === 0 || undefined}>
                    {canEdit ? (
                      <button type="button" className="ppl-roster-cell" data-cell={`${pi}:${di}`} tabIndex={focus.person === pi && focus.day === di ? 0 : -1} aria-label={label} onFocus={() => setFocus({ person: pi, day: di })} onClick={() => setOpen({ person: pi, day: di })}>
                        {text}
                      </button>
                    ) : (
                      <span aria-label={label}>{text}</span>
                    )}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );

  return (
    <div className="ppl-page">
      <header className="ppl-head">
        <div>
          <Nav />
          {report && (
            <p className="page-sub">
              {isolate(report.meta.roster.title)} · {isolate(span(report.meta.roster.from_date, report.meta.roster.to_date))} {statusBadge(report.meta.roster.status)}
              {report.meta.roster.drafts > 0 && <span className="faint"> {report.meta.roster.drafts === 1 ? t("1 draft not published") : t("{n} drafts not published", { n: report.meta.roster.drafts })}</span>}
            </p>
          )}
        </div>
        {report && canPlan && (
          <div className="ppl-tools">
            <Button onClick={() => setFilling(true)} disabled={!!publishing}>
              {t("Fill with a shift")}
            </Button>
            <Button variant="primary" onClick={() => void publish()} loading={!!publishing} disabled={!report.meta.roster.drafts}>
              {t("Publish")}
            </Button>
          </div>
        )}
      </header>
      {publishing && <p aria-live="polite">{publishing}</p>}
      {failure && <Alert tone="error">{isolate(failure)}</Alert>}
      {body}
      {report && open && people[open.person] && <CellDialog report={report} person={people[open.person]} day={days[open.day]} roster={id} onClose={() => setOpen(null)} onDone={done} />}
      {report && filling && <FillDialog report={report} onClose={() => setFilling(false)} onDone={done} />}
    </div>
  );
}
