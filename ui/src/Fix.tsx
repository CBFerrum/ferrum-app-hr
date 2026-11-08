// Asking for attendance to be put right: a forgotten check-in or check-out, a day (or several)
// worked from home or away on duty. The request goes to the approver, who decides; the server
// refuses what cannot be asked for (a day to come, a holiday, a day of leave) and says why next to
// the field it is about.

import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { api, ApiError, errorMessage, todayIso, t, isolate } from "@ferrum/client";
import { Alert, Button, Dialog, Field, toast } from "@ferrum/kit";

const REASONS = [
  { value: "Missed Check-in", get label() { return t("I forgot to check in or out"); } },
  { value: "Work From Home", get label() { return t("I worked from home"); } },
  { value: "On Duty", get label() { return t("I was on duty away from the office"); } },
];

export function Fix({ employee, day, onClose }: { employee: string; day: string | null; onClose: () => void }) {
  const qc = useQueryClient();
  const today = todayIso();
  const [reason, setReason] = useState(REASONS[0].value);
  const [from, setFrom] = useState(day ?? today);
  const [to, setTo] = useState(day ?? today);
  const [several, setSeveral] = useState(false);
  const [half, setHalf] = useState(false);
  const [came, setCame] = useState("");
  const [left, setLeft] = useState("");
  const [what, setWhat] = useState("");
  const [busy, setBusy] = useState(false);
  const [problems, setProblems] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setProblems({});
    setError(null);
    if (!what.trim()) return setProblems({ explanation: t("Say what happened, so that your approver can decide") });
    setBusy(true);
    try {
      const made = await api.post<{ approver?: string | null }>(
        "/api/v1/AttendanceRequest",
        {
          employee,
          reason,
          from_date: from,
          to_date: several ? to : from,
          half_day: half,
          in_time: !several && came ? came : null,
          out_time: !several && left ? left : null,
          explanation: what.trim(),
        },
        { idempotencyKey: crypto.randomUUID() },
      );
      toast(made.approver ? t("Sent to {approver} for approval", { approver: made.approver }) : t("Sent for approval"));
      void qc.invalidateQueries({ queryKey: ["people"] });
      onClose();
    } catch (err) {
      if (err instanceof ApiError && err.fields.length) {
        setProblems(Object.fromEntries(err.fields.map((x) => [x.field, x.message])));
        setError(err.fields.some((x) => !["from_date", "to_date", "in_time", "out_time", "explanation"].includes(x.field)) ? errorMessage(err) : null);
      } else setError(errorMessage(err));
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title={t("Ask for attendance to be put right")}
      footer={
        <>
          <Button onClick={onClose}>{t("Cancel")}</Button>
          <Button type="submit" form="fix-form" variant="primary" loading={busy}>
            {t("Send for approval")}
          </Button>
        </>
      }
    >
      <form id="fix-form" className="stack" onSubmit={(e) => void submit(e)} noValidate>
        {error && <Alert tone="error">{isolate(error)}</Alert>}
        <Field label={t("What happened?")} htmlFor="fix-reason">
          <select id="fix-reason" className="select" value={reason} onChange={(e) => setReason(e.target.value)}>
            {REASONS.map((r) => (
              <option key={r.value} value={r.value}>
                {isolate(r.label)}
              </option>
            ))}
          </select>
        </Field>
        <div className="ppl-fix-days">
          <Field label={several ? t("From") : t("Day")} htmlFor="fix-from" error={problems.from_date}>
            <input id="fix-from" className="input" type="date" max={today} required value={from} onChange={(e) => setFrom(e.target.value)} />
          </Field>
          {several && (
            <Field label={t("To")} htmlFor="fix-to" error={problems.to_date}>
              <input id="fix-to" className="input" type="date" max={today} min={from} required value={to} onChange={(e) => setTo(e.target.value)} />
            </Field>
          )}
        </div>
        <label className="check-field row">
          <input type="checkbox" checked={several} onChange={(e) => setSeveral(e.target.checked)} /> {t("More than one day")}
        </label>
        {!several && (
          <div className="ppl-fix-days">
            <Field label={t("Came in at")} htmlFor="fix-in" error={problems.in_time} hint={t("Optional")}>
              <input id="fix-in" className="input" type="time" value={came} onChange={(e) => setCame(e.target.value)} />
            </Field>
            <Field label={t("Left at")} htmlFor="fix-out" error={problems.out_time} hint={t("Optional")}>
              <input id="fix-out" className="input" type="time" value={left} onChange={(e) => setLeft(e.target.value)} />
            </Field>
          </div>
        )}
        <label className="check-field row">
          <input type="checkbox" checked={half} onChange={(e) => setHalf(e.target.checked)} /> {(several ? t("Only half of the first day") : t("Only half of the day"))}
        </label>
        <Field label={t("Tell your approver more")} htmlFor="fix-what" required error={problems.explanation}>
          <textarea id="fix-what" className="textarea" rows={3} value={what} maxLength={2000} onChange={(e) => setWhat(e.target.value)} />
        </Field>
      </form>
    </Dialog>
  );
}
