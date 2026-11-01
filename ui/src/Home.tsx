// A person's own space, People's home: checking in, what is left of their leave, what is coming up
// (their time off and the holidays), their requests of every kind in one list, the month's
// attendance, their payslips and their documents. Everything is theirs: it reads what their own
// rights reach. On a phone it is one column in the order a day goes. A manager sees, first, how many
// requests wait for them.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { lazy, Suspense, useCallback, useState } from "react";
import { Link } from "react-router";
import { api, errorMessage, todayIso } from "@ferrum/client";
import { Badge, Button, EmptyState, ErrorState, Spinner, statusTone, toast, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { useMe } from "@ferrum/records";
import type { Holiday } from "./calendar";
import { f, rows, title, useCanRead } from "./data";
import { addDays, span } from "./dates";
import { Nav, useInbox } from "./Nav";
import { days, nextLogType, type Checkin, type Employee } from "./people";
import { type BalanceRow, comingUp, meter, requests, type Time, when } from "./space";

const Month = lazy(() => import("./Month").then((m) => ({ default: m.Month })));
const Pay = lazy(() => import("./Pay").then((m) => ({ default: m.Pay })));
const Fix = lazy(() => import("./Fix").then((m) => ({ default: m.Fix })));

/** The signed-in person's employee record, if there is one. */
function useMine(email: string | undefined) {
  return useQuery({
    queryKey: ["people", "me", email],
    enabled: !!email,
    queryFn: ({ signal }) =>
      rows<Employee>("Employee", { filters: f([["user", "=", email]]), limit: 1, fields: "employee_name,department,designation,date_of_joining,user" }, signal).then((r) => r.rows[0] ?? null),
  });
}

function CheckIn({ employee }: { employee: Employee }) {
  const qc = useQueryClient();
  const today = todayIso();
  const last = useQuery({
    queryKey: ["people", "checkin", employee.id],
    queryFn: ({ signal }) => rows<Checkin>("EmployeeCheckin", { filters: f([["employee", "=", employee.id]]), sort: "log_time desc", limit: 1, fields: "log_type,log_time" }, signal).then((r) => r.rows[0]),
  });
  const [busy, setBusy] = useState(false);
  if (last.isLoading) return <Spinner small label="Loading your check-in" />;
  const next = nextLogType(last.data, today);
  const record = async () => {
    setBusy(true);
    try {
      await api.post("/api/v1/EmployeeCheckin", { employee: employee.id, log_type: next }, { idempotencyKey: crypto.randomUUID() });
      toast(next === "IN" ? "Checked in" : "Checked out");
      await qc.invalidateQueries({ queryKey: ["people", "checkin", employee.id] });
    } catch (err) {
      toast(errorMessage(err), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="row ppl-checkin">
      <Button variant={next === "IN" ? "primary" : "default"} loading={busy} onClick={() => void record()}>
        {next === "IN" ? "Check in" : "Check out"}
      </Button>
      {last.data?.log_time && (
        <span className="faint">
          Last {last.data.log_type === "IN" ? "in" : "out"} {new Date(last.data.log_time).toLocaleString()}
        </span>
      )}
    </div>
  );
}

function Balances({ employee }: { employee: Employee }) {
  const q = useQuery({
    queryKey: ["people", "balance", employee.id],
    queryFn: ({ signal }) => api.get<{ rows: BalanceRow[] }>("/api/reports/leave_balance", { employee: employee.id }, signal).then((r) => r.rows),
  });
  if (q.error) return <ErrorState error={q.error} title="Your leave cannot be shown" onRetry={() => void q.refetch()} />;
  if (!q.data) return <Spinner label="Loading your leave" />;
  if (!q.data.length) return <p className="muted">No leave is allocated to you for this period yet.</p>;
  return (
    <ul className="ppl-balances" data-ferrum-nocard>
      {q.data.map((b) => {
        const m = meter(b);
        return (
          <li key={b.leave_type} className="ppl-balance">
            <div className="ppl-balance-head">
              <strong className="ppl-balance-days">{days(b.available)}</strong>
              <span>{b.leave_type}</span>
            </div>
            <div className="ppl-meter" role="img" aria-label={`${b.leave_type}: ${days(b.allocated)} in all, ${days(b.pending)} waiting for approval, ${days(b.available)} left`}>
              <span data-part="spent" style={{ width: `${m.spent}%` }} />
              <span data-part="asked" style={{ width: `${m.asked}%` }} />
              <span data-part="left" style={{ width: `${m.left}%` }} />
            </div>
            <span className="faint">
              of {days(b.allocated)}
              {Number(b.pending) > 0 && ` · ${days(b.pending)} waiting for approval`}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

/** The next time off, and the holidays to come. */
function Coming({ employee }: { employee: Employee }) {
  const today = todayIso();
  const q = useQuery({
    queryKey: ["people", "coming", employee.id, today],
    queryFn: async ({ signal }) => {
      const [leave, cal] = await Promise.all([
        rows<Time>("LeaveApplication", { filters: f([["employee", "=", employee.id], ["to_date", ">=", today], ["docstatus", "<", 2]]), sort: "from_date asc", limit: 20, fields: "leave_type,from_date,to_date,status" }, signal),
        api.get<{ holidays: Holiday[] }>("/api/app/hr/calendar", { from: today, to: addDays(today, 60) }, signal).catch(() => ({ holidays: [] as Holiday[] })),
      ]);
      return comingUp(
        leave.rows.map((l) => ({ ...l, leave_type: title(l, "leave_type") ?? l.leave_type })),
        cal.holidays,
        today,
      );
    },
  });
  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <Spinner label="Loading what is coming up" />;
  if (!q.data.length) return <p className="muted">No time off or holidays in the next two months.</p>;
  return (
    <ul className="ppl-coming" data-ferrum-nocard>
      {q.data.map((u) => (
        <li key={u.key} data-kind={u.kind}>
          <span className="ppl-when">{span(u.from, u.to)}</span>
          <span className="ppl-coming-what">
            {u.label}
            {u.note && <span className="faint"> · {u.note}</span>}
          </span>
          <span className="faint">{when(u, today)}</span>
        </li>
      ))}
    </ul>
  );
}

/** Leave, attendance requests and expense claims together. */
function Requests({ employee, onFix }: { employee: Employee; onFix: () => void }) {
  const attendance = useCanRead("AttendanceRequest");
  const claims = useCanRead("ExpenseClaim");
  const q = useQuery({
    queryKey: ["people", "requests", employee.id, attendance, claims],
    enabled: attendance !== undefined && claims !== undefined,
    queryFn: async ({ signal }) => {
      const mine = f([["employee", "=", employee.id]]);
      const [leave, asked, claimed] = await Promise.all([
        rows<{ id: string; leave_type?: string; from_date: string; to_date: string; status: string }>("LeaveApplication", { filters: mine, sort: "from_date desc", limit: 6, fields: "leave_type,from_date,to_date,status" }, signal),
        attendance
          ? rows<{ id: string; reason?: string; from_date: string; to_date: string; status: string }>("AttendanceRequest", { filters: mine, sort: "from_date desc", limit: 6, fields: "reason,from_date,to_date,status" }, signal)
          : Promise.resolve({ rows: [] }),
        claims
          ? rows<{ id: string; posting_date: string; total_claimed_amount?: string; status: string }>("ExpenseClaim", { filters: mine, sort: "posting_date desc", limit: 6, fields: "posting_date,total_claimed_amount,status" }, signal)
          : Promise.resolve({ rows: [] }),
      ]);
      return requests(
        leave.rows.map((l) => ({ ...l, leave_type: title(l, "leave_type") ?? l.leave_type })),
        asked.rows,
        claimed.rows,
      );
    },
  });
  return (
    <div className="stack ppl-tight">
      <div className="row ppl-actions">
        <Link className="btn btn-primary btn-sm" to={`/e/LeaveApplication/new?employee=${employee.id}`}>
          Ask for leave
        </Link>
        {attendance && (
          <Button size="sm" onClick={onFix}>
            Fix attendance
          </Button>
        )}
        {claims && (
          <Link className="btn btn-default btn-sm" to={`/e/ExpenseClaim/new?employee=${employee.id}`}>
            Claim expenses
          </Link>
        )}
      </div>
      {q.error ? (
        <ErrorState error={q.error} onRetry={() => void q.refetch()} />
      ) : !q.data ? (
        <Spinner label="Loading your requests" />
      ) : !q.data.length ? (
        <p className="muted">You have not asked for anything yet.</p>
      ) : (
        <ul className="ppl-requests" data-ferrum-nocard>
          {q.data.map((r) => (
            <li key={r.key}>
              <span className="faint ppl-kind">{r.kind}</span>
              <Link to={`/e/${r.entity}/${r.id}`}>{r.title}</Link>
              <Badge tone={statusTone(r.status)}>{r.status}</Badge>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Section({ id, heading, children }: { id: string; heading: string; children: React.ReactNode }) {
  return (
    <section className="card" aria-labelledby={id}>
      <div className="card-head">
        <h2 id={id}>{heading}</h2>
      </div>
      <div className="card-body">{children}</div>
    </section>
  );
}

const Loading = ({ label }: { label: string }) => <Spinner label={label} />;

export function PeopleHome() {
  useShellTitle("My space");
  const qc = useQueryClient();
  const me = useMe().data;
  const mine = useMine(me?.user.email);
  const waiting = useInbox().data?.length ?? 0;
  const [fixing, setFixing] = useState<string | null | false>(false);
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["people"] }), [qc]);
  useLiveEntity("LeaveApplication", refresh);
  useLiveEntity("AttendanceRequest", refresh);
  useLiveEntity("EmployeeCheckin", refresh);
  useLiveEntity("Attendance", refresh);

  const person = mine.data;
  const sub = person ? [title(person, "designation"), title(person, "department")].filter(Boolean).join(" · ") : "";
  return (
    <div className="ppl-page">
      <header className="ppl-head">
        <div>
          <Nav />
          <p className="page-sub">{person ? [person.employee_name, sub].filter(Boolean).join(" · ") : " "}</p>
        </div>
      </header>
      {waiting > 0 && (
        <Link className="ppl-banner" to="/approvals">
          <strong>{waiting === 1 ? "1 request is" : `${waiting} requests are`} waiting for your decision</strong>
          <span>Review them</span>
        </Link>
      )}
      {mine.error ? (
        <ErrorState error={mine.error} onRetry={() => void mine.refetch()} />
      ) : mine.isLoading || !me ? (
        <Spinner label="Loading your space" />
      ) : !person ? (
        <EmptyState title="Your sign-in is not linked to an employee record" icon="user">
          <p className="muted">HR can link it. Then your leave, attendance and payslips are here.</p>
          <Link to="/calendar">See who is away</Link>
        </EmptyState>
      ) : (
        <div className="ppl-space">
          <div className="ppl-col">
            <Section id="ppl-today" heading="Today">
              <CheckIn employee={person} />
            </Section>
            <Section id="ppl-leave" heading="Your leave">
              <Balances employee={person} />
            </Section>
            <Section id="ppl-coming" heading="Coming up">
              <Coming employee={person} />
            </Section>
            <Section id="ppl-requests" heading="Your requests">
              <Requests employee={person} onFix={() => setFixing(null)} />
            </Section>
          </div>
          <div className="ppl-col">
            <Suspense fallback={<Loading label="Loading your attendance" />}>
              <Month employee={person} onFix={(day) => setFixing(day)} />
              <Pay employee={person} />
            </Suspense>
          </div>
        </div>
      )}
      {fixing !== false && person && (
        <Suspense fallback={null}>
          <Fix employee={person.id} day={fixing} onClose={() => setFixing(false)} />
        </Suspense>
      )}
    </div>
  );
}
