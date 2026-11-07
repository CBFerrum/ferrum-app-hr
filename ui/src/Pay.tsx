// What is theirs on paper: the payslips HR has made for them, the documents kept on their record,
// and the training they have passed and until when it counts. Each reads only what their own rights
// reach; a part they cannot read is left out, not shown as broken.

import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router";
import { api, docPath, download, errorMessage, formatDate, todayIso, t as tr, isolate } from "@ferrum/client";
import { Badge, EmptyState, ErrorState, Icon, Spinner, toast } from "@ferrum/kit";
import { f, rows, title, useCanRead } from "./data";
import { money } from "./inbox";
import type { Employee } from "./people";
import { cover, slipPeriod } from "./space";

interface Slip {
  id: string;
  start_date: string;
  end_date: string;
  net_pay?: string;
  gross_pay?: string;
}

function Payslips({ employee }: { employee: Employee }) {
  const q = useQuery({
    queryKey: ["people", "slips", employee.id],
    queryFn: ({ signal }) =>
      rows<Slip>("SalarySlip", { filters: f([["employee", "=", employee.id], ["docstatus", "=", 1]]), sort: "start_date desc", limit: 6, fields: "start_date,end_date,net_pay,gross_pay,currency" }, signal).then((r) => r.rows),
  });
  return (
    <section className="card" aria-labelledby="ppl-slips">
      <div className="card-head">
        <h2 id="ppl-slips">{tr("Your payslips")}</h2>
      </div>
      <div className="card-body">
        {q.error ? (
          <ErrorState error={q.error} onRetry={() => void q.refetch()} />
        ) : !q.data ? (
          <Spinner label={tr("Loading your payslips")} />
        ) : !q.data.length ? (
          <p className="muted">{tr("No payslips yet. They appear here once payroll has run.")}</p>
        ) : (
          <ul className="ppl-slips" data-ferrum-nocard>
            {q.data.map((s) => (
              <li key={s.id}>
                <Link to={`/e/SalarySlip/${s.id}`}>{isolate(slipPeriod(s.start_date, s.end_date))}</Link>
                <span className="ppl-num">
                  <strong>{isolate(money(s.net_pay, title(s, "currency") ?? ""))}</strong>
                  <span className="faint"> {tr("net")}</span>
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

interface Attached {
  id: string;
  file_name: string;
  url: string;
  size_bytes: number;
  created_at: string;
}

const kb = (n: number) => (n >= 1_048_576 ? tr("{value} MB", { value: (n / 1_048_576).toFixed(1) }) : tr("{n} KB", { n: Math.max(1, Math.round(n / 1024)) }));

function Documents({ employee, training }: { employee: Employee; training: boolean }) {
  const today = todayIso();
  const files = useQuery({
    queryKey: ["people", "documents", employee.id],
    queryFn: ({ signal }) => api.get<{ attachments: Attached[] }>(docPath("Employee", employee.id, "attachments"), undefined, signal).then((r) => r.attachments),
  });
  const passed = useQuery({
    queryKey: ["people", "training", employee.id],
    enabled: training,
    queryFn: ({ signal }) =>
      rows<{ id: string; program_name?: string; completed_on?: string; valid_until?: string | null }>("TrainingRecord", { filters: f([["employee", "=", employee.id]]), sort: "completed_on desc", limit: 10, fields: "program_name,completed_on,valid_until" }, signal).then(
        (r) => r.rows,
      ),
  });
  return (
    <section className="card" aria-labelledby="ppl-docs">
      <div className="card-head">
        <h2 id="ppl-docs">{tr("Documents and training")}</h2>
      </div>
      <div className="card-body stack ppl-tight">
        {files.error ? (
          <p className="muted">{tr("Your documents cannot be shown (")}{isolate(errorMessage(files.error))}).</p>
        ) : !files.data ? (
          <Spinner small label={tr("Loading your documents")} />
        ) : !files.data.length ? (
          <p className="muted">{tr("HR has not put any documents on your record.")}</p>
        ) : (
          <ul className="ppl-files" data-ferrum-nocard>
            {files.data.map((d) => (
              <li key={d.id}>
                <button type="button" className="link-btn row" onClick={() => download(d.url, d.file_name).catch((e) => toast(errorMessage(e), "error"))}>
                  <Icon name="download" size={14} />
                  <span>{isolate(d.file_name)}</span>
                </button>
                <span className="faint">{isolate(kb(d.size_bytes))}</span>
              </li>
            ))}
          </ul>
        )}
        {training && passed.data && passed.data.length > 0 && (
          <ul className="ppl-training" data-ferrum-nocard aria-label={tr("Training you have passed")}>
            {passed.data.map((t) => {
              const c = cover(t.valid_until, today);
              return (
                <li key={t.id}>
                  <span>{isolate(t.program_name ?? "")}</span>
                  <span className="faint">{t.valid_until ? tr("until {value}", { value: formatDate(t.valid_until) }) : t.completed_on ? tr("passed {value}", { value: formatDate(t.completed_on) }) : ""}</span>
                  {c === "expired" && <Badge tone="red">{tr("Lapsed")}</Badge>}
                  {c === "soon" && <Badge tone="amber">{tr("Lapses soon")}</Badge>}
                </li>
              );
            })}
          </ul>
        )}
        {training && passed.error && <EmptyState title={tr("Your training cannot be shown")} />}
      </div>
    </section>
  );
}

export function Pay({ employee }: { employee: Employee }) {
  const slips = useCanRead("SalarySlip");
  const training = useCanRead("TrainingRecord");
  return (
    <>
      {slips && <Payslips employee={employee} />}
      <Documents employee={employee} training={!!training} />
    </>
  );
}
