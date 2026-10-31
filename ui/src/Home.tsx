// People's home: for the signed-in person, their leave balances, check-in and own requests; the
// leave requests waiting for their approval; and, for those who see everyone, who is away today,
// who joined lately, the work anniversaries coming up, and the directory.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useState } from "react";
import { Link } from "react-router";
import { api, errorMessage } from "@ferrum/client";
import { Badge, Button, EmptyState, ErrorState, Spinner, statusTone, TabPanel, Tabs, toast, useDebounced, useLiveEntity, useShellTitle } from "@ferrum/kit";
import { useMe, useMetaIndex } from "@ferrum/records";
import { anniversaries, covers, days, type Employee, inDaysLabel, type Leave, localDay, nextLogType, type Checkin } from "./people";

const rows = <T,>(entity: string, query: Record<string, string | number>, signal?: AbortSignal) =>
  api.get<{ rows: T[]; total?: number }>(`/api/v1/${entity}`, { titles: "true", ...query }, signal);
const f = (filters: unknown[]) => JSON.stringify(filters);
const title = (r: { _titles?: Record<string, string> }, field: string) => r._titles?.[field];
const when = (from?: string | null, to?: string | null) => (from === to || !to ? (from ?? "") : `${from} – ${to}`);

interface Balance {
  leave_type: string;
  available: string;
  balance: string;
  pending: string;
}

function useMine(me: string | undefined) {
  return useQuery({
    queryKey: ["people", "me", me],
    enabled: !!me,
    queryFn: ({ signal }) => rows<Employee & { company?: string }>("Employee", { filters: f([["user", "=", me]]), limit: 1, fields: "employee_name,department,designation,date_of_joining,user" }, signal).then((r) => r.rows[0] ?? null),
  });
}

function CheckIn({ employee }: { employee: Employee }) {
  const qc = useQueryClient();
  const today = localDay(new Date());
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
    queryFn: ({ signal }) => api.get<{ rows: Balance[] }>("/api/reports/leave_balance", { employee: employee.id }, signal).then((r) => r.rows),
  });
  if (q.error) return <p className="muted">Your leave balance cannot be shown ({errorMessage(q.error)}).</p>;
  if (!q.data) return <Spinner small label="Loading your leave balance" />;
  if (!q.data.length) return <p className="muted">No leave allocated to you this period.</p>;
  return (
    <div className="ppl-balances">
      {q.data.map((b) => (
        <div key={b.leave_type} className="ppl-balance">
          <strong>{days(b.available)}</strong>
          <span>{b.leave_type}</span>
          {Number(b.pending) > 0 && <span className="faint">{days(b.pending)} waiting for approval</span>}
        </div>
      ))}
    </div>
  );
}

function MyLeave({ employee }: { employee: Employee }) {
  const q = useQuery({
    queryKey: ["people", "my-leave", employee.id],
    queryFn: ({ signal }) =>
      rows<Leave>("LeaveApplication", { filters: f([["employee", "=", employee.id]]), sort: "from_date desc", limit: 5, fields: "leave_type,from_date,to_date,status,total_leave_days" }, signal).then((r) => r.rows),
  });
  if (q.error) return null;
  if (!q.data) return <Spinner small label="Loading your requests" />;
  if (!q.data.length) return <p className="muted">You have not asked for leave yet.</p>;
  return (
    <ul className="stack" data-ferrum-nocard>
      {q.data.map((l) => (
        <li key={l.id}>
          <Link to={`e/LeaveApplication/${l.id}`}>{title(l, "leave_type") ?? "Leave"}</Link> <span className="faint">{when(l.from_date, l.to_date)} · {days(l.total_leave_days)}</span>{" "}
          <Badge tone={statusTone(l.status)}>{l.status}</Badge>
        </li>
      ))}
    </ul>
  );
}

function Me({ employee }: { employee: Employee }) {
  return (
    <section className="card" aria-labelledby="ppl-me">
      <div className="card-head">
        <h2 id="ppl-me">{employee.employee_name}</h2>
        <span className="faint">{[title(employee, "designation"), title(employee, "department")].filter(Boolean).join(" · ")}</span>
      </div>
      <div className="card-body stack">
        <CheckIn employee={employee} />
        <Balances employee={employee} />
        <div className="row ppl-head-row">
          <h3>Your leave</h3>
          <Link to={`e/LeaveApplication/new?employee=${employee.id}`}>
            <Button size="sm" icon="plus">
              Ask for leave
            </Button>
          </Link>
        </div>
        <MyLeave employee={employee} />
      </div>
    </section>
  );
}

function Approvals({ me }: { me: string }) {
  const qc = useQueryClient();
  const q = useQuery({
    queryKey: ["people", "approvals", me],
    queryFn: ({ signal }) =>
      rows<Leave>("LeaveApplication", { filters: f([["leave_approver", "=", me], ["status", "=", "Open"]]), sort: "from_date asc", limit: 50, fields: "employee_name,leave_type,from_date,to_date,total_leave_days,status" }, signal).then(
        (r) => r.rows,
      ),
  });
  const [busy, setBusy] = useState<string | null>(null);
  const act = async (l: Leave, action: "Approve" | "Reject") => {
    setBusy(l.id);
    try {
      await api.post(`/api/v1/LeaveApplication/${l.id}/workflow/${action}`);
      toast(`${action === "Approve" ? "Approved" : "Rejected"}: ${l.employee_name}, ${when(l.from_date, l.to_date)}`);
      await qc.invalidateQueries({ queryKey: ["people"] });
    } catch (err) {
      toast(errorMessage(err), "error");
    } finally {
      setBusy(null);
    }
  };
  if (q.error || !q.data?.length) return null;
  return (
    <section className="card" aria-labelledby="ppl-approvals">
      <div className="card-head">
        <h2 id="ppl-approvals">Waiting for your approval ({q.data.length})</h2>
      </div>
      <div className="card-body">
        <ul className="ppl-requests" data-ferrum-nocard>
          {q.data.map((l) => (
            <li key={l.id}>
              <span>
                <Link to={`e/LeaveApplication/${l.id}`}>{l.employee_name}</Link>{" "}
                <span className="faint">
                  {title(l, "leave_type") ?? "Leave"} · {when(l.from_date, l.to_date)} · {days(l.total_leave_days)}
                </span>
              </span>
              <span className="row">
                <Button size="sm" variant="primary" loading={busy === l.id} onClick={() => void act(l, "Approve")}>
                  Approve
                </Button>
                <Button size="sm" disabled={busy === l.id} onClick={() => void act(l, "Reject")}>
                  Reject
                </Button>
              </span>
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}

function Today() {
  const today = localDay(new Date());
  const monthAgo = localDay(new Date(Date.now() - 30 * 86_400_000));
  const away = useQuery({
    queryKey: ["people", "away", today],
    queryFn: ({ signal }) =>
      rows<Leave>("LeaveApplication", { filters: f([["status", "=", "Approved"], ["from_date", "<=", today], ["to_date", ">=", today]]), limit: 100, fields: "employee,employee_name,leave_type,from_date,to_date,half_day" }, signal).then((r) =>
        r.rows.filter((l) => covers(l, today)),
      ),
  });
  const team = useQuery({
    queryKey: ["people", "active"],
    queryFn: ({ signal }) => rows<Employee>("Employee", { filters: f([["status", "=", "Active"]]), limit: 500, count: "true", fields: "employee_name,date_of_joining,department" }, signal),
  });
  if (away.error || team.error) return <ErrorState error={away.error ?? team.error} onRetry={() => void Promise.all([away.refetch(), team.refetch()])} />;
  if (!away.data || !team.data) return <Spinner label="Loading today" />;
  const joined = team.data.rows.filter((e) => (e.date_of_joining ?? "") >= monthAgo && (e.date_of_joining ?? "") <= today);
  const coming = anniversaries(team.data.rows, today);
  return (
    <div className="ppl-columns">
      <section className="card" aria-labelledby="ppl-away">
        <div className="card-head">
          <h2 id="ppl-away">Away today ({away.data.length})</h2>
          <span className="faint">of {team.data.total ?? team.data.rows.length} people</span>
        </div>
        <div className="card-body">
          {away.data.length ? (
            <ul className="stack">
              {away.data.map((l) => (
                <li key={l.id}>
                  {l.employee_name} <span className="faint">{title(l, "leave_type")}{l.half_day ? ", half a day" : ""}, until {l.to_date}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="muted">Everyone is in.</p>
          )}
        </div>
      </section>
      <section className="card" aria-labelledby="ppl-joined">
        <div className="card-head">
          <h2 id="ppl-joined">Joined this month</h2>
        </div>
        <div className="card-body">
          {joined.length ? (
            <ul className="stack">
              {joined.map((e) => (
                <li key={e.id}>
                  <Link to={`e/Employee/${e.id}`}>{e.employee_name}</Link> <span className="faint">{[title(e, "department"), e.date_of_joining].filter(Boolean).join(" · ")}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="muted">Nobody new in the last 30 days.</p>
          )}
        </div>
      </section>
      <section className="card" aria-labelledby="ppl-anniversaries">
        <div className="card-head">
          <h2 id="ppl-anniversaries">Work anniversaries</h2>
        </div>
        <div className="card-body">
          {coming.length ? (
            <ul className="stack">
              {coming.map((a) => (
                <li key={a.employee.id}>
                  {a.employee.employee_name}{" "}
                  <span className="faint">
                    {a.years} {a.years === 1 ? "year" : "years"}, {inDaysLabel(a.inDays)}
                  </span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="muted">None in the next two weeks.</p>
          )}
        </div>
      </section>
    </div>
  );
}

function Directory() {
  const [q, setQ] = useState("");
  const search = useDebounced(q.trim(), 250);
  const list = useQuery({
    queryKey: ["people", "directory", search],
    queryFn: ({ signal }) =>
      rows<Employee>("Employee", { filters: f([["status", "=", "Active"]]), ...(search ? { search } : {}), sort: "employee_name asc", limit: 60, count: "true", fields: "employee_name,department,designation,company_email,cell_number" }, signal),
  });
  return (
    <div className="stack">
      <input className="input ppl-search" type="search" placeholder="Search people by name" aria-label="Search people" value={q} onChange={(e) => setQ(e.target.value)} />
      {list.error ? (
        <ErrorState error={list.error} onRetry={() => void list.refetch()} />
      ) : !list.data ? (
        <Spinner label="Loading people" />
      ) : !list.data.rows.length ? (
        <EmptyState title={search ? `Nobody matches “${search}”` : "No one here yet"} icon="user" />
      ) : (
        <>
          <ul className="ppl-directory" data-ferrum-nocard>
            {list.data.rows.map((e) => (
              <li key={e.id} className="ppl-person">
                <span className="ppl-avatar" aria-hidden="true">
                  {(e.employee_name ?? "?")
                    .split(/\s+/)
                    .slice(0, 2)
                    .map((w) => w[0]?.toUpperCase())
                    .join("")}
                </span>
                <span className="ppl-person-text">
                  <Link to={`e/Employee/${e.id}`}>{e.employee_name}</Link>
                  <span className="faint">{[title(e, "designation"), title(e, "department")].filter(Boolean).join(" · ")}</span>
                  {e.company_email && <a href={`mailto:${e.company_email}`}>{e.company_email}</a>}
                  {e.cell_number && <a href={`tel:${e.cell_number}`}>{e.cell_number}</a>}
                </span>
              </li>
            ))}
          </ul>
          {(list.data.total ?? 0) > list.data.rows.length && <p className="muted">Showing {list.data.rows.length} of {list.data.total}: search to find someone.</p>}
        </>
      )}
    </div>
  );
}

export function PeopleHome() {
  useShellTitle("People");
  const me = useMe().data?.user.email;
  const meta = useMetaIndex().data;
  const read = (e: string) => meta?.entities.find((x) => x.name === e)?.permissions.read ?? "none";
  const everyone = read("Employee") === "all";
  const mine = useMine(me);
  const [tab, setTab] = useState<"overview" | "directory">("overview");
  const qc = useQueryClient();
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: ["people"] }), [qc]);
  useLiveEntity("LeaveApplication", refresh);
  useLiveEntity("EmployeeCheckin", refresh);
  if (meta && read("Employee") === "none") {
    return (
      <EmptyState title="Nothing here for you yet" icon="user">
        <p className="muted">Ask an administrator for an HR role, or to link your sign-in to your employee record.</p>
      </EmptyState>
    );
  }
  const overview = (
    <div className="stack">
      {mine.data && <Me employee={mine.data} />}
      {mine.data === null && !everyone && <p className="muted">Your sign-in is not linked to an employee record yet; HR can link it.</p>}
      {me && read("LeaveApplication") !== "none" && <Approvals me={me} />}
      {everyone && <Today />}
    </div>
  );
  return (
    <div className="stack">
      <div className="page-head">
        <div>
          <h1 className="page-title">People</h1>
          <p className="page-sub">{everyone ? "Your leave and check-ins, what waits for you, and who is in today." : "Your leave and check-ins."}</p>
        </div>
      </div>
      {everyone ? (
        <>
          <Tabs
            id="ppl"
            value={tab}
            onChange={(v) => setTab(v as "overview" | "directory")}
            tabs={[
              { id: "overview", label: "Overview" },
              { id: "directory", label: "Directory" },
            ]}
          />
          <TabPanel id="ppl" tab={tab}>
            {tab === "overview" ? overview : <Directory />}
          </TabPanel>
        </>
      ) : (
        overview
      )}
    </div>
  );
}
