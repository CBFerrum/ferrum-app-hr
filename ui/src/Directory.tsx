// The people of the company: a directory to search and an org chart to open a level at a time. The
// directory is name, role, department, place and work email for everyone; phone numbers and the
// joiners and anniversaries of the month only for those who see every employee. The chart opens
// from the people who answer to nobody, each card with how many answer to it; search jumps to
// someone and opens the lines above them; the keyboard walks it as a tree.

import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { api, bootstrap, routeBase, todayIso } from "@ferrum/client";
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
        {marks(p.name)}
      </span>
      <span className="ppl-person-text">
        {detail ? <Link to={`/e/Employee/${p.id}`}>{p.name}</Link> : <strong>{p.name}</strong>}
        <span className="faint">{place(p)}</span>
        {p.branch && <span className="faint">{p.branch}</span>}
        {p.email && <a href={`mailto:${p.email}`}>{p.email}</a>}
        {detail && p.phone && <a href={`tel:${p.phone}`}>{p.phone}</a>}
      </span>
    </li>
  );
}

function List({ data, q, department }: { data: Directory; q: string; department: string }) {
  const found = useMemo(() => find(data.people, q).filter((p) => !department || p.department === department), [data.people, q, department]);
  if (!found.length) return <EmptyState title={q || department ? "Nobody matches" : "No one here yet"} icon="user" />;
  return (
    <>
      <ul className="ppl-directory" data-ferrum-nocard>
        {found.slice(0, 120).map((p) => (
          <Card key={p.id} p={p} detail={data.detail} />
        ))}
      </ul>
      {found.length > 120 && <p className="muted">Showing 120 of {found.length}: search to narrow them down.</p>}
      {data.truncated && <p className="muted">The company is larger than this list can hold: search finds only those shown.</p>}
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
        {marks(p.name)}
      </span>
      <span className="ppl-node-text">
        {link ? (
          <Link to={`/e/Employee/${p.id}`} tabIndex={-1}>
            {p.name}
          </Link>
        ) : (
          <strong>{p.name}</strong>
        )}
        <span className="faint">{place(p) || "No role set"}</span>
      </span>
      {node.children.length > 0 && (
        <button
          type="button"
          className="ppl-reports"
          tabIndex={-1}
          aria-label={`${open ? "Hide" : "Show"} the ${node.children.length} ${node.children.length === 1 ? "person" : "people"} who report to ${p.name}`}
          onClick={onToggle}
        >
          {node.children.length}
          <span className="faint">{n > node.children.length ? ` of ${n}` : ""}</span>
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

  if (!data.people.length) return <EmptyState title="No one here yet" icon="user" />;
  return (
    <div className="stack ppl-tight">
      <div className="row">
        <Button size="sm" onClick={() => setOpen(everyone(tree))}>
          Open every line
        </Button>
        <Button size="sm" onClick={() => setOpen(new Set())}>
          Close them
        </Button>
        <span className="faint">{data.people.length} people</span>
      </div>
      {matches.length > 0 && (
        <ul className="ppl-found" aria-label="People found" data-ferrum-nocard>
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
                <strong>{m.name}</strong> <span className="faint">{place(m)}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      <ul className="ppl-tree" role="tree" aria-label="Who reports to whom" data-ferrum-nocard>
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
      <p className="faint">{rows.length} shown. The arrow keys walk the chart: right opens a line, left closes it.</p>
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
          <h2 id="ppl-joined">Joined this month</h2>
        </div>
        <div className="card-body">
          {joined.length ? (
            <ul className="stack ppl-tight" data-ferrum-nocard>
              {joined.map((e) => (
                <li key={e.id}>
                  <Link to={`/e/Employee/${e.id}`}>{e.employee_name}</Link> <span className="faint">{[e.department, e.date_of_joining].filter(Boolean).join(" · ")}</span>
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
            <ul className="stack ppl-tight" data-ferrum-nocard>
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
    </>
  );
}

/** Where the public careers page is, for HR to share. */
function Careers() {
  const url = `${window.location.origin}${routeBase("people")}careers/${bootstrap()?.workspace.slug ?? ""}`;
  return (
    <section className="card" aria-labelledby="ppl-careers">
      <div className="card-head">
        <h2 id="ppl-careers">Your careers page</h2>
      </div>
      <div className="card-body stack ppl-tight">
        <p className="muted">Anyone can read your published openings here and apply, without signing in.</p>
        <input className="input" readOnly aria-label="Address of the careers page" value={url} onFocus={(e) => e.currentTarget.select()} />
        <div className="row">
          <Button
            size="sm"
            onClick={() => void navigator.clipboard.writeText(url).then(() => toast("Address copied"), () => toast("Select the address and copy it", "error"))}
          >
            Copy the address
          </Button>
          <a className="btn btn-default btn-sm" href={url} target="_blank" rel="noreferrer">
            Open it
          </a>
          <Link className="btn btn-default btn-sm" to="/e/JobOpening">
            Openings
          </Link>
        </div>
      </div>
    </section>
  );
}

export function People() {
  useShellTitle("People");
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
          <p className="page-sub">{dir.data ? `${dir.data.people.length} people` : " "}</p>
        </div>
      </header>
      <div className="ppl-tools">
        <div className="segmented" role="group" aria-label="Show">
          <button type="button" aria-pressed={view === "list"} onClick={() => set({ view: null })}>
            Directory
          </button>
          <button type="button" aria-pressed={view === "chart"} onClick={() => set({ view: "chart" })}>
            Org chart
          </button>
        </div>
        <input
          className="input ppl-search"
          type="search"
          placeholder={view === "chart" ? "Find someone in the chart" : "Search by name, role or department"}
          aria-label="Search people"
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        {view === "list" && departments.length > 1 && (
          <select className="select ppl-dept" aria-label="Department" value={department} onChange={(e) => set({ department: e.target.value || null })}>
            <option value="">All departments</option>
            {departments.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        )}
      </div>
      {dir.error ? (
        <ErrorState error={dir.error} onRetry={() => void dir.refetch()} />
      ) : !dir.data ? (
        <Spinner label="Loading people" />
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
