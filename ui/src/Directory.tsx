// The people of the company: a directory to search and an org chart to open a level at a time. The
// directory is name, role, department, place and work email for everyone; phone numbers and the
// joiners and anniversaries of the month only for those who see every employee. The chart opens
// from the people who answer to nobody, each card with how many answer to it; search jumps to
// someone and opens the lines above them; the keyboard walks it as a tree.

import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { api, bootstrap, routeBase, todayIso, t, isolate } from "@ferrum/client";
import { Button, EmptyState, ErrorState, Spinner, toast, useDebounced, useShellTitle } from "@ferrum/kit";
import { useMetaIndex } from "@ferrum/records";
import { addDays } from "./dates";
import { Nav } from "./Nav";
import { anniversaries, inDaysLabel } from "./people";
import { above, everyone, find, forest, marks, type Node, type Person, showing, size } from "./orgchart";

interface Directory {
  people: Person[];
  truncated: boolean;
  /** Whether this person sees every employee: phone numbers and joining dates come with it. */
  detail: boolean;
}

const place = (p: Person) => [p.designation, p.department].filter(Boolean).join(" · ");

function Card({ p, detail }: { p: Person; detail: boolean }) {
  return (
    <li className="ppl-person">
      <span className="ppl-avatar" aria-hidden="true">
        {isolate(marks(p.name))}
      </span>
      <span className="ppl-person-text">
        {detail ? <Link to={`/e/Employee/${p.id}`}>{isolate(p.name ?? "")}</Link> : <strong>{isolate(p.name ?? "")}</strong>}
        <span className="faint">{isolate(place(p))}</span>
        {p.branch && <span className="faint">{isolate(p.branch)}</span>}
        {p.email && <a href={`mailto:${p.email}`}>{isolate(p.email)}</a>}
        {detail && p.phone && <a href={`tel:${p.phone}`}>{isolate(p.phone)}</a>}
      </span>
    </li>
  );
}

function List({ data, q, department }: { data: Directory; q: string; department: string }) {
  const found = useMemo(() => find(data.people, q).filter((p) => !department || p.department === department), [data.people, q, department]);
  if (!found.length) return <EmptyState title={q || department ? t("Nobody matches") : t("No one here yet")} icon="user" />;
  return (
    <>
      <ul className="ppl-directory" data-ferrum-nocard>
        {found.slice(0, 120).map((p) => (
          <Card key={p.id} p={p} detail={data.detail} />
        ))}
      </ul>
      {found.length > 120 && <p className="muted">{t("Showing 120 of {length}: search to narrow them down.", { length: found.length })}</p>}
      {data.truncated && <p className="muted">{t("The company is larger than this list can hold: search finds only those shown.")}</p>}
    </>
  );
}

/** One row of the chart, as a tree item: its card, and the count of those who answer to it. */
function Row({
  node,
  depth,
  open,
  focus,
  link,
  onToggle,
  onKey,
  onFocus,
}: {
  node: Node;
  depth: number;
  open: boolean;
  focus: boolean;
  link: boolean;
  onToggle: () => void;
  onKey: (e: React.KeyboardEvent) => void;
  onFocus: () => void;
}) {
  const p = node.person;
  const n = size(node);
  return (
    <li
      id={`node-${p.id}`}
      role="treeitem"
      aria-level={depth + 1}
      aria-expanded={node.children.length ? open : undefined}
      tabIndex={focus ? 0 : -1}
      className="ppl-node"
      style={{ "--depth": depth } as React.CSSProperties}
      onKeyDown={onKey}
      onFocus={onFocus}
    >
      <span className="ppl-avatar" aria-hidden="true">
        {isolate(marks(p.name))}
      </span>
      <span className="ppl-node-text">
        {link ? (
          <Link to={`/e/Employee/${p.id}`} tabIndex={-1}>
            {isolate(p.name ?? "")}
          </Link>
        ) : (
          <strong>{isolate(p.name ?? "")}</strong>
        )}
        <span className="faint">{place(p) || t("No role set")}</span>
      </span>
      {node.children.length > 0 && (
        <button
          type="button"
          className="ppl-reports"
          tabIndex={-1}
          aria-label={t("{value} the {length} {value2} who report to {name}", { value: open ? "Hide" : "Show", length: node.children.length, value2: node.children.length === 1 ? "person" : "people", name: String(p.name) })}
          onClick={onToggle}
        >
          {node.children.length}
          <span className="faint">{n > node.children.length ? t(" of {n}", { n }) : ""}</span>
        </button>
      )}
    </li>
  );
}

function Chart({ data, q }: { data: Directory; q: string }) {
  const tree = useMemo(() => forest(data.people), [data.people]);
  const [open, setOpen] = useState<Set<string>>(new Set());
  const [focus, setFocus] = useState<string | null>(null);
  const [jump, setJump] = useState<string | null>(null);
  const rows = showing(tree, open);
  const at = Math.max(0, rows.findIndex((r) => r.node.person.id === focus));
  const matches = q.trim() ? find(data.people, q).slice(0, 8) : [];

  const toggle = (id: string) =>
    setOpen((o) => {
      const next = new Set(o);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const go = (i: number) => {
    const r = rows[Math.min(rows.length - 1, Math.max(0, i))];
    if (r) {
      setFocus(r.node.person.id);
      document.getElementById(`node-${r.node.person.id}`)?.focus();
    }
  };
  // Keys as the tree pattern has them: up and down walk the rows, right opens a line and enters it,
  // left closes it and leaves it; Enter and Space open or close the line the keyboard is on.
  const key = (e: React.KeyboardEvent, i: number) => {
    const r = rows[i];
    const hasKids = r.node.children.length > 0;
    const expanded = open.has(r.node.person.id);
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        return go(i + 1);
      case "ArrowUp":
        e.preventDefault();
        return go(i - 1);
      case "Home":
        e.preventDefault();
        return go(0);
      case "End":
        e.preventDefault();
        return go(rows.length - 1);
      case "ArrowRight":
        e.preventDefault();
        if (hasKids && !expanded) toggle(r.node.person.id);
        else if (hasKids) go(i + 1);
        return;
      case "ArrowLeft": {
        e.preventDefault();
        if (hasKids && expanded) return toggle(r.node.person.id);
        const parent = [...rows.slice(0, i)].reverse().find((x) => x.depth < r.depth);
        return parent && go(rows.indexOf(parent));
      }
      case "Enter":
      case " ":
        e.preventDefault();
        if (hasKids) toggle(r.node.person.id);
        return;
    }
  };

  // Someone found by search: the lines above them open, and the keyboard is on them.
  useEffect(() => {
    if (!jump) return;
    document.getElementById(`node-${jump}`)?.scrollIntoView({ block: "center" });
    document.getElementById(`node-${jump}`)?.focus();
    setJump(null);
  }, [jump, rows.length]);

  if (!data.people.length) return <EmptyState title={t("No one here yet")} icon="user" />;
  return (
    <div className="stack ppl-tight">
      <div className="row">
        <Button size="sm" onClick={() => setOpen(everyone(tree))}>
          {t("Open every line")}
        </Button>
        <Button size="sm" onClick={() => setOpen(new Set())}>
          {t("Close them")}
        </Button>
        <span className="faint">{t("{length} people", { length: data.people.length })}</span>
      </div>
      {matches.length > 0 && (
        <ul className="ppl-found" aria-label={t("People found")} data-ferrum-nocard>
          {matches.map((m) => (
            <li key={m.id}>
              <button
                type="button"
                onClick={() => {
                  setOpen((o) => new Set([...o, ...above(data.people, m.id)]));
                  setFocus(m.id);
                  setJump(m.id);
                }}
              >
                <strong>{isolate(m.name ?? "")}</strong> <span className="faint">{isolate(place(m))}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      <ul className="ppl-tree" role="tree" aria-label={t("Who reports to whom")} data-ferrum-nocard>
        {rows.map((r, i) => (
          <Row
            key={r.node.person.id}
            node={r.node}
            depth={r.depth}
            open={open.has(r.node.person.id)}
            focus={i === at}
            link={data.detail}
            onToggle={() => toggle(r.node.person.id)}
            onKey={(e) => key(e, i)}
            onFocus={() => setFocus(r.node.person.id)}
          />
        ))}
      </ul>
      <p className="faint">{t("{length} shown. The arrow keys walk the chart: right opens a line, left closes it.", { length: rows.length })}</p>
    </div>
  );
}

/** Joined in the last 30 days and the anniversaries of the next two weeks: for those who see every employee. */
function Milestones({ people }: { people: Person[] }) {
  const today = todayIso();
  const monthAgo = addDays(today, -30);
  const staff = people.map((p) => ({ id: p.id, employee_name: p.name, date_of_joining: p.joined ?? null, department: p.department }));
  const joined = staff.filter((e) => (e.date_of_joining ?? "") >= monthAgo && (e.date_of_joining ?? "") <= today);
  const coming = anniversaries(staff, today);
  return (
    <>
      <section className="card" aria-labelledby="ppl-joined">
        <div className="card-head">
          <h2 id="ppl-joined">{t("Joined this month")}</h2>
        </div>
        <div className="card-body">
          {joined.length ? (
            <ul className="stack ppl-tight" data-ferrum-nocard>
              {joined.map((e) => (
                <li key={e.id}>
                  <Link to={`/e/Employee/${e.id}`}>{isolate(e.employee_name ?? "")}</Link> <span className="faint">{isolate([e.department, e.date_of_joining].filter(Boolean).join(" · "))}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="muted">{t("Nobody new in the last 30 days.")}</p>
          )}
        </div>
      </section>
      <section className="card" aria-labelledby="ppl-anniversaries">
        <div className="card-head">
          <h2 id="ppl-anniversaries">{t("Work anniversaries")}</h2>
        </div>
        <div className="card-body">
          {coming.length ? (
            <ul className="stack ppl-tight" data-ferrum-nocard>
              {coming.map((a) => (
                <li key={a.employee.id}>
                  {isolate(a.employee.employee_name ?? "")}{isolate(" ")}
                  <span className="faint">
                    {a.years} {a.years === 1 ? t("year") : t("years")}, {isolate(inDaysLabel(a.inDays))}
                  </span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="muted">{t("None in the next two weeks.")}</p>
          )}
        </div>
      </section>
    </>
  );
}

/** Where the public careers page is, for HR to share. */
function Careers() {
  const url = `${window.location.origin}${routeBase("people")}careers/${bootstrap()?.workspace.slug ?? ""}`;
  return (
    <section className="card" aria-labelledby="ppl-careers">
      <div className="card-head">
        <h2 id="ppl-careers">{t("Your careers page")}</h2>
      </div>
      <div className="card-body stack ppl-tight">
        <p className="muted">{t("Anyone can read your published openings here and apply, without signing in.")}</p>
        <input className="input" readOnly aria-label={t("Address of the careers page")} value={url} onFocus={(e) => e.currentTarget.select()} />
        <div className="row">
          <Button
            size="sm"
            onClick={() => void navigator.clipboard.writeText(url).then(() => toast(t("Address copied")), () => toast(t("Select the address and copy it"), "error"))}
          >
            {t("Copy the address")}
          </Button>
          <a className="btn btn-default btn-sm" href={url} target="_blank" rel="noreferrer">
            {t("Open it")}
          </a>
          <Link className="btn btn-default btn-sm" to="/e/JobOpening">
            {t("Openings")}
          </Link>
        </div>
      </div>
    </section>
  );
}

export function People() {
  useShellTitle(t("People"));
  const [sp, setSp] = useSearchParams();
  const view = sp.get("view") === "chart" ? "chart" : "list";
  const [text, setText] = useState(sp.get("q") ?? "");
  const q = useDebounced(text, 200);
  const department = sp.get("department") ?? "";
  const meta = useMetaIndex().data;
  // HR, who write openings, are given the address of the careers page to share.
  const hr = (meta?.entities.find((e) => e.name === "JobOpening")?.permissions.write ?? "none") !== "none";
  const dir = useQuery({ queryKey: ["people", "directory"], queryFn: ({ signal }) => api.get<Directory>("/api/app/hr/directory", undefined, signal) });

  // The address follows what is typed, once it settles.
  useEffect(() => {
    const next = new URLSearchParams(sp);
    q.trim() ? next.set("q", q.trim()) : next.delete("q");
    if (next.toString() !== sp.toString()) setSp(next, { replace: true });
    // Only the settled text moves the address; the rest of the address is read, not driven, here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [q]);
  const set = (change: Record<string, string | null>) => {
    const next = new URLSearchParams(sp);
    for (const [k, v] of Object.entries(change)) v ? next.set(k, v) : next.delete(k);
    setSp(next, { replace: true });
  };
  const aside = !!dir.data && (dir.data.detail || hr);
  const departments = useMemo(() => [...new Set((dir.data?.people ?? []).map((p) => p.department).filter((d): d is string => !!d))].sort((a, b) => a.localeCompare(b)), [dir.data]);

  return (
    <div className="ppl-page">
      <header className="ppl-head">
        <div>
          <Nav />
          <p className="page-sub">{dir.data ? t("{length} people", { length: dir.data.people.length }) : " "}</p>
        </div>
      </header>
      <div className="ppl-tools">
        <div className="segmented" role="group" aria-label={t("Show")}>
          <button type="button" aria-pressed={view === "list"} onClick={() => set({ view: null })}>
            {t("Directory")}
          </button>
          <button type="button" aria-pressed={view === "chart"} onClick={() => set({ view: "chart" })}>
            {t("Org chart")}
          </button>
        </div>
        <input
          className="input ppl-search"
          type="search"
          placeholder={view === "chart" ? t("Find someone in the chart") : t("Search by name, role or department")}
          aria-label={t("Search people")}
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        {view === "list" && departments.length > 1 && (
          <select className="select ppl-dept" aria-label={t("Department")} value={department} onChange={(e) => set({ department: e.target.value || null })}>
            <option value="">{t("All departments")}</option>
            {departments.map((d) => (
              <option key={d} value={d}>
                {isolate(d)}
              </option>
            ))}
          </select>
        )}
      </div>
      {dir.error ? (
        <ErrorState error={dir.error} onRetry={() => void dir.refetch()} />
      ) : !dir.data ? (
        <Spinner label={t("Loading people")} />
      ) : (
        <div className={aside ? "ppl-people-grid" : undefined}>
          <div className="stack ppl-tight">{view === "chart" ? <Chart data={dir.data} q={q} /> : <List data={dir.data} q={q} department={department} />}</div>
          {aside && (
            <div className="stack ppl-aside">
              {dir.data.detail && <Milestones people={dir.data.people} />}
              {hr && <Careers />}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
