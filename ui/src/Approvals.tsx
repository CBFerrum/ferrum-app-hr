// What waits for you to decide: leave, attendance requests and expense claims routed to you, the
// oldest first, each with why it was asked for and what a decision needs beside it (what the leave
// leaves in the balance and who else is away; what the days hold now; what was claimed against what
// is sanctioned). Approving is one click; rejecting asks for a note, which goes to the request's
// discussion. A decision shows at once and the card comes back, with the server's reason, if the
// server refuses it. The server holds the rules (who may decide, the balance, the versions).

import { useQueryClient } from "@tanstack/react-query";
import { type FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { api, docPath, errorMessage } from "@ferrum/client";
import { Badge, Button, Dialog, EmptyState, ErrorState, Field, Spinner, toast, useShellTitle } from "@ferrum/kit";
import {
  badSanctions,
  balanceLine,
  claimedTimes,
  counts,
  type ExpenseItem,
  type Item,
  KINDS,
  type Kind,
  kindLabel,
  money,
  only,
  recordedLine,
  sanctionedTotal,
  stale,
  teamLine,
  waiting,
  what,
  withSanctions,
} from "./inbox";
import { INBOX, Nav, useInbox } from "./Nav";
import { marks } from "./orgchart";

const keyOf = (i: Item) => `${i.entity}/${i.id}`;
const byWaiting = (a: Item, b: Item) => (a.waiting_since ?? "").localeCompare(b.waiting_since ?? "");

type Edits = Record<string, Record<string, string>>;

/** The claim's lines, with a sanctioned amount the approver can lower, as a form of their own. */
function Lines({ item, edits, onEdit }: { item: ExpenseItem; edits: Record<string, string>; onEdit: (line: string, value: string) => void }) {
  const s = item.summary;
  const bad = badSanctions(s.lines, edits);
  const total = sanctionedTotal(s.lines, edits);
  return (
    <div className="ppl-lines">
      <div className="ppl-line ppl-line-head" aria-hidden="true">
        <span>What for</span>
        <span className="ppl-num">Claimed</span>
        <span className="ppl-num">Sanction</span>
      </div>
      {s.lines.map((l) => {
        const id = `sanction-${item.id}-${l.id}`;
        return (
          <div key={l.id} className="ppl-line">
            <label htmlFor={id}>
              {l.type ?? "Expense"}
              {l.description && <span className="faint"> · {l.description}</span>}
            </label>
            <span className="ppl-num" aria-label={`Claimed ${money(l.amount, s.currency, s.precision)}`}>
              {money(l.amount, s.currency, s.precision)}
            </span>
            <span className="ppl-num">
              <input
                id={id}
                className="input ppl-sanction"
                inputMode="decimal"
                value={edits[l.id] ?? String(l.sanctioned ?? l.amount ?? "")}
                aria-invalid={bad[l.id] ? true : undefined}
                aria-describedby={bad[l.id] ? `${id}-error` : undefined}
                onChange={(e) => onEdit(l.id, e.target.value)}
              />
            </span>
            {bad[l.id] && (
              <span className="field-error ppl-line-error" id={`${id}-error`}>
                {bad[l.id]}
              </span>
            )}
          </div>
        );
      })}
      <div className="ppl-line ppl-line-total">
        <span>To pay once sanctioned</span>
        <span className="ppl-num">{money(s.claimed, s.currency, s.precision)}</span>
        <span className="ppl-num">
          <strong>{money(total, s.currency, s.precision)}</strong>
        </span>
      </div>
      {Number(s.advance) > 0 && <p className="faint ppl-line-note">{money(s.advance, s.currency, s.precision)} of it is set off against an advance already paid.</p>}
    </div>
  );
}

function Context({ item }: { item: Item }) {
  if (item.kind === "leave") {
    return (
      <ul className="ppl-context" data-ferrum-nocard>
        {balanceLine(item) && <li>{balanceLine(item)}</li>}
        {teamLine(item) && <li>{teamLine(item)}</li>}
      </ul>
    );
  }
  if (item.kind === "attendance") {
    const says = claimedTimes(item);
    return (
      <ul className="ppl-context" data-ferrum-nocard>
        {says && <li>Says: {says}.</li>}
        {recordedLine(item) && <li>The days hold: {recordedLine(item)}</li>}
      </ul>
    );
  }
  return null;
}

function Card({
  item,
  cursor,
  picked,
  edits,
  onPick,
  onEdit,
  onApprove,
  onReject,
}: {
  item: Item;
  cursor: boolean;
  picked: boolean;
  edits: Record<string, string>;
  onPick: (on: boolean) => void;
  onEdit: (line: string, value: string) => void;
  onApprove: () => void;
  onReject: () => void;
}) {
  const key = keyOf(item);
  const name = item.employee.name ?? "Someone";
  const ref = useRef<HTMLLIElement>(null);
  // The keyboard's place stays in view.
  useEffect(() => {
    if (cursor) ref.current?.scrollIntoView({ block: "nearest" });
  }, [cursor]);
  const bad = item.kind === "expense" && Object.keys(badSanctions(item.summary.lines, edits)).length > 0;
  return (
    <li ref={ref} className="ppl-card" data-cursor={cursor || undefined} data-kind={item.kind} aria-labelledby={`who-${key}`}>
      <div className="ppl-card-head">
        <input type="checkbox" className="ppl-check" checked={picked} aria-label={`Select ${name}'s request`} onChange={(e) => onPick(e.target.checked)} />
        <span className="ppl-avatar" aria-hidden="true">
          {marks(name)}
        </span>
        <div className="ppl-card-who">
          <strong id={`who-${key}`}>{name}</strong>
          {item.employee.department && <span className="faint">{item.employee.department}</span>}
        </div>
        <Badge tone={item.kind === "leave" ? "blue" : item.kind === "attendance" ? "purple" : "amber"}>{kindLabel(item.kind)}</Badge>
        <span className={stale(item.waiting_since) ? "ppl-late" : "faint"} title="How long it has waited">
          {waiting(item.waiting_since)}
        </span>
      </div>
      <p className="ppl-card-what">{what(item)}</p>
      {item.reason && <blockquote className="ppl-reason">{item.reason}</blockquote>}
      <Context item={item} />
      {item.kind === "expense" && <Lines item={item} edits={edits} onEdit={onEdit} />}
      <div className="ppl-card-actions">
        <Button variant="primary" size="sm" icon="check" disabled={bad} onClick={onApprove} aria-label={`Approve ${name}'s ${item.kind === "expense" ? "claim" : "request"}`}>
          Approve
        </Button>
        <Button size="sm" onClick={onReject} aria-label={`Reject ${name}'s ${item.kind === "expense" ? "claim" : "request"}`}>
          Reject…
        </Button>
        <Link className="ppl-open" to={`/e/${item.entity}/${item.id}`}>
          Open the request
        </Link>
      </div>
    </li>
  );
}

/** Rejecting asks why, in words the employee will read. */
function RejectDialog({ item, onClose, onReject }: { item: Item; onClose: () => void; onReject: (note: string) => void }) {
  const [note, setNote] = useState("");
  const submit = (e: FormEvent) => {
    e.preventDefault();
    onReject(note);
  };
  return (
    <Dialog open onClose={onClose} title={`Reject ${item.employee.name ?? "this"}'s request?`}>
      <form className="stack" onSubmit={submit}>
        <p className="muted">{what(item)}</p>
        <Field label="Why not? (optional)" htmlFor="reject-note" hint="Added to the request's discussion, where the employee reads it.">
          <textarea id="reject-note" className="textarea" rows={3} value={note} maxLength={2000} onChange={(e) => setNote(e.target.value)} data-autofocus />
        </Field>
        <div className="row ppl-dialog-actions">
          <Button onClick={onClose}>Keep it waiting</Button>
          <Button type="submit" variant="danger">
            Reject
          </Button>
        </div>
      </form>
    </Dialog>
  );
}

export function Inbox() {
  useShellTitle("Approvals");
  const qc = useQueryClient();
  const q = useInbox();
  const [sp, setSp] = useSearchParams();
  const kind = KINDS.some((k) => k.id === sp.get("kind")) ? (sp.get("kind") as Kind) : null;
  const [edits, setEdits] = useState<Edits>({});
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [cursor, setCursor] = useState<string | null>(null);
  const [rejecting, setRejecting] = useState<Item | null>(null);
  const [working, setWorking] = useState(false);

  const all = q.data ?? [];
  const shown = useMemo(() => only(all, kind), [all, kind]);
  const by = counts(all);
  const kinds = KINDS.filter((k) => by[k.id] > 0);
  // What is ticked and still waiting: a request decided on its own card is no longer ticked.
  const chosen = all.filter((i) => picked.has(keyOf(i)));
  const unsound = (i: Item) => i.kind === "expense" && Object.keys(badSanctions(i.summary.lines, edits[keyOf(i)] ?? {})).length > 0;

  // A card leaves at once; if the server refuses, it returns with the reason.
  const take = useCallback((i: Item) => qc.setQueryData<Item[]>(INBOX, (old) => old?.filter((x) => keyOf(x) !== keyOf(i))), [qc]);
  const restore = useCallback((i: Item) => qc.setQueryData<Item[]>(INBOX, (old = []) => (old.some((x) => keyOf(x) === keyOf(i)) ? old : [...old, i].sort(byWaiting))), [qc]);

  /** Saves the sanctioned amounts the approver changed, on the version they saw: if the employee
   *  has changed the claim since, the server refuses and the card comes back to be read again. */
  const sanction = async (i: ExpenseItem, changes: Record<string, string>): Promise<number> => {
    const claim = await api.get<{ expenses: { id?: string; sanctioned_amount?: unknown }[] }>(docPath("ExpenseClaim", i.id), { titles: "false" });
    const saved = await api.patch<{ version: number }>(docPath("ExpenseClaim", i.id), { expenses: withSanctions(claim.expenses, changes) }, { ifMatch: i.version });
    return saved.version;
  };

  const decide = async (i: Item, action: "Approve" | "Reject", note = ""): Promise<boolean> => {
    const name = i.employee.name ?? "the request";
    take(i);
    try {
      let version = i.version;
      const changes = edits[keyOf(i)];
      if (action === "Approve" && i.kind === "expense" && changes && Object.keys(changes).length) version = await sanction(i, changes);
      await api.post(docPath(i.entity, i.id, "workflow", action), undefined, { ifMatch: version });
      if (action === "Reject" && note.trim()) {
        // The decision stands even if the note cannot be saved.
        await api.post(docPath(i.entity, i.id, "comments"), { body: `Rejected: ${note.trim()}`, mentions: [] }).catch(() => toast("Rejected, but the note could not be saved.", "error"));
      }
      toast(action === "Approve" ? `Approved: ${name}` : `Rejected: ${name}`);
      // What the decision changed elsewhere (a balance, the calendar, the attendance) is read again.
      void qc.invalidateQueries({ queryKey: ["people"] });
      return true;
    } catch (e) {
      restore(i);
      toast(errorMessage(e), "error");
      void qc.invalidateQueries({ queryKey: INBOX });
      return false;
    }
  };

  const approveSome = async (items: Item[]) => {
    const ready = items.filter((i) => !unsound(i));
    setWorking(true);
    let done = 0;
    for (const i of ready) if (await decide(i, "Approve")) done++;
    setPicked(new Set());
    setWorking(false);
    if (items.length > 1) toast(`Approved ${done} of ${items.length}${ready.length < items.length ? `; fix the amounts on ${items.length - ready.length} first` : ""}`);
  };

  // J and K move, A approves and R rejects the one you are on. Typing is never taken for a shortcut.
  const at = shown.findIndex((i) => keyOf(i) === cursor);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (e.metaKey || e.ctrlKey || e.altKey || rejecting || t?.closest('input:not([type="checkbox"]), textarea, select, dialog, [contenteditable="true"]')) return;
      const k = e.key.toLowerCase();
      if (k === "j" || k === "k") {
        if (!shown.length) return;
        e.preventDefault();
        const next = k === "j" ? Math.min(shown.length - 1, at + 1) : Math.max(0, at < 0 ? 0 : at - 1);
        setCursor(keyOf(shown[next]));
      } else if ((k === "a" || k === "r") && at >= 0) {
        e.preventDefault();
        const i = shown[at];
        if (k === "r") setRejecting(i);
        else if (!unsound(i)) {
          // Move on to the next card before this one leaves.
          setCursor(keyOf(shown[at + 1] ?? shown[at - 1] ?? i));
          void decide(i, "Approve");
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const body = q.error ? (
    <ErrorState error={q.error} onRetry={() => void q.refetch()} />
  ) : !q.data ? (
    <Spinner label="Loading what is waiting for you" />
  ) : !all.length ? (
    <EmptyState title="Nothing is waiting for you" icon="check">
      <p className="muted">Leave, attendance and expense requests routed to you appear here as they come in.</p>
      <Link to="/calendar">See who is away</Link>
    </EmptyState>
  ) : (
    <>
      {kinds.length > 1 && (
        <div className="segmented" role="group" aria-label="Kind of request">
          <button type="button" aria-pressed={!kind} onClick={() => setSp({}, { replace: true })}>
            All ({all.length})
          </button>
          {kinds.map((k) => (
            <button key={k.id} type="button" aria-pressed={kind === k.id} onClick={() => setSp({ kind: k.id }, { replace: true })}>
              {k.label} ({by[k.id]})
            </button>
          ))}
        </div>
      )}
      {chosen.length > 0 && (
        <div className="ppl-bulk" role="region" aria-label="Selected requests">
          <strong>{chosen.length} selected</strong>
          <Button variant="primary" size="sm" icon="check" loading={working} onClick={() => void approveSome(chosen)}>
            Approve {chosen.length === 1 ? "it" : "them"}
          </Button>
          <Button size="sm" onClick={() => setPicked(new Set())}>
            Clear
          </Button>
        </div>
      )}
      {!shown.length && <p className="muted">Nothing of this kind is waiting for you.</p>}
      <ul className="ppl-cards" aria-label="Requests waiting for you">
        {shown.map((i) => (
          <Card
            key={keyOf(i)}
            item={i}
            cursor={cursor === keyOf(i)}
            picked={picked.has(keyOf(i))}
            edits={edits[keyOf(i)] ?? {}}
            onPick={(on) => setPicked((p) => (on ? new Set(p).add(keyOf(i)) : new Set([...p].filter((x) => x !== keyOf(i)))))}
            onEdit={(line, value) => setEdits((e) => ({ ...e, [keyOf(i)]: { ...e[keyOf(i)], [line]: value } }))}
            onApprove={() => void decide(i, "Approve")}
            onReject={() => setRejecting(i)}
          />
        ))}
      </ul>
      <p className="faint ppl-keys">J and K move between requests, A approves the one you are on, R rejects it.</p>
    </>
  );

  return (
    <div className="ppl-page ppl-approvals">
      <header className="ppl-head">
        <div>
          <Nav />
          <p className="page-sub" aria-live="polite">
            {q.data ? (all.length ? `${all.length} waiting for you, the oldest first` : "You are up to date") : " "}
          </p>
        </div>
      </header>
      {body}
      {rejecting && (
        <RejectDialog
          item={rejecting}
          onClose={() => setRejecting(null)}
          onReject={(note) => {
            const i = rejecting;
            setRejecting(null);
            void decide(i, "Reject", note);
          }}
        />
      )}
    </div>
  );
}
