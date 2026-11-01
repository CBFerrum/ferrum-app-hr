// Who reports to whom: the people as a forest of reporting lines, opened a level at a time, and
// the search that finds someone in it. Pure, so it is tested without a browser.

export interface Person {
  id: string;
  name: string | null;
  designation?: string | null;
  department?: string | null;
  branch?: string | null;
  manager?: string | null;
  email?: string | null;
  phone?: string | null;
  joined?: string | null;
}

export interface Node {
  person: Person;
  children: Node[];
}

const byName = (a: Person, b: Person) => (a.name ?? "").localeCompare(b.name ?? "") || a.id.localeCompare(b.id);

/** Who each person answers to. Nothing stops A reporting to B and B to A (or someone to
 *  themselves), so a loop is cut at its first person by name, who then answers to nobody: every
 *  line has a top, and nobody is drawn twice. A manager who is not among the people is no one. */
export function lines(people: Person[]): Map<string, string | null> {
  const known = new Map(people.map((p) => [p.id, p]));
  const manager = new Map<string, string | null>(people.map((p) => [p.id, p.manager && p.manager !== p.id && known.has(p.manager) ? p.manager : null]));
  const state = new Map<string, 1 | 2>();
  for (const start of [...people].sort(byName)) {
    const path: string[] = [];
    let at: string | null = start.id;
    while (at && !state.has(at)) {
      state.set(at, 1);
      path.push(at);
      at = manager.get(at) ?? null;
    }
    // Back on the way just walked: a loop, from where it closed.
    if (at && state.get(at) === 1) {
      const head = path
        .slice(path.indexOf(at))
        .map((id) => known.get(id)!)
        .sort(byName)[0];
      manager.set(head.id, null);
    }
    path.forEach((id) => state.set(id, 2));
  }
  return manager;
}

/** The lines of reporting, opened from the people who answer to nobody. */
export function forest(people: Person[]): Node[] {
  const manager = lines(people);
  const under = new Map<string, Person[]>();
  const roots: Person[] = [];
  for (const p of people) {
    const m = manager.get(p.id);
    if (m) under.set(m, [...(under.get(m) ?? []), p]);
    else roots.push(p);
  }
  const grow = (p: Person): Node => ({ person: p, children: (under.get(p.id) ?? []).sort(byName).map(grow) });
  return roots.sort(byName).map(grow);
}

/** The people above someone, from the top down (not including them). */
export function above(people: Person[], id: string): string[] {
  const manager = lines(people);
  const chain: string[] = [];
  for (let m = manager.get(id); m; m = manager.get(m)) chain.unshift(m);
  return chain;
}

/** A node and, for those it is open on, what is under it, in the order a page reads: the rows that
 *  are showing, each with its depth. */
export interface Shown {
  node: Node;
  depth: number;
}

export function showing(nodes: Node[], open: ReadonlySet<string>, depth = 0): Shown[] {
  return nodes.flatMap((n) => [{ node: n, depth }, ...(open.has(n.person.id) ? showing(n.children, open, depth + 1) : [])]);
}

/** How many people report to someone, all the way down. */
export function size(node: Node): number {
  return node.children.reduce((n, c) => n + 1 + size(c), 0);
}

/** Everyone with a manager's reports open: for "Expand all". */
export function everyone(nodes: Node[]): Set<string> {
  const out = new Set<string>();
  const walk = (ns: Node[]) =>
    ns.forEach((n) => {
      if (n.children.length) out.add(n.person.id);
      walk(n.children);
    });
  walk(nodes);
  return out;
}

/** The people a search finds: every word in a name, role, department or place, by name. */
export function find(people: Person[], query: string): Person[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return [...people].sort(byName);
  return people
    .filter((p) => {
      const hay = [p.name, p.designation, p.department, p.branch, p.email].filter(Boolean).join(" ").toLowerCase();
      return words.every((w) => hay.includes(w));
    })
    .sort(byName);
}

/** "Asha Rao" → "AR". */
export function marks(name: string | null | undefined): string {
  const parts = (name ?? "").trim().split(/\s+/).filter(Boolean);
  return ((parts[0]?.[0] ?? "?") + (parts.length > 1 ? parts[parts.length - 1][0] : "")).toUpperCase();
}
