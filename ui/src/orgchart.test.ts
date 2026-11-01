import { describe, expect, it } from "vitest";
import { above, everyone, find, forest, marks, type Person, showing, size } from "./orgchart";

const p = (id: string, manager: string | null = null, extra: Partial<Person> = {}): Person => ({ id, name: id.toUpperCase(), manager, ...extra });

describe("the org chart", () => {
  const people = [p("d", "b"), p("b", "a"), p("c", "a"), p("a"), p("e", "gone"), p("f", "b")];

  it("lines people up under their managers, by name", () => {
    const tree = forest(people);
    expect(tree.map((n) => n.person.id)).toEqual(["a", "e"]);
    expect(tree[0].children.map((n) => n.person.id)).toEqual(["b", "c"]);
    expect(tree[0].children[0].children.map((n) => n.person.id)).toEqual(["d", "f"]);
    expect(size(tree[0])).toBe(4);
  });

  it("opens a level at a time", () => {
    const tree = forest(people);
    const ids = (open: string[]) => showing(tree, new Set(open)).map((s) => `${s.depth}${s.node.person.id}`);
    expect(ids([])).toEqual(["0a", "0e"]);
    expect(ids(["a"])).toEqual(["0a", "1b", "1c", "0e"]);
    expect(ids(["a", "b"])).toEqual(["0a", "1b", "2d", "2f", "1c", "0e"]);
    expect([...everyone(tree)].sort()).toEqual(["a", "b"]);
  });

  it("does not loop when two people report to each other, or someone to themselves", () => {
    const tangled = [p("x", "y"), p("y", "x"), p("z", "z"), p("w", "x")];
    const tree = forest(tangled);
    const all: string[] = [];
    const walk = (ns: typeof tree) => ns.forEach((n) => (all.push(n.person.id), walk(n.children)));
    walk(tree);
    expect(all.sort()).toEqual(["w", "x", "y", "z"]);
    // The loop is cut at its first person by name, and what hangs from it hangs from there.
    expect(tree.map((n) => n.person.id)).toEqual(["x", "z"]);
    expect(tree[0].children.map((n) => n.person.id)).toEqual(["w", "y"]);
    expect(above(tangled, "w")).toEqual(["x"]);
    expect(above(tangled, "y")).toEqual(["x"]);
    expect(above(tangled, "x")).toEqual([]);
  });

  it("names everyone above someone, from the top", () => {
    expect(above(people, "d")).toEqual(["a", "b"]);
    expect(above(people, "a")).toEqual([]);
    expect(above(people, "e")).toEqual([]);
    expect(above(people, "nobody")).toEqual([]);
  });

  it("finds people by every word, in what the cards show", () => {
    const staff = [p("a", null, { name: "Asha Rao", designation: "Support Lead", department: "Support" }), p("b", null, { name: "Ben Ito", designation: "Engineer", department: "Support" })];
    expect(find(staff, "support lead").map((x) => x.id)).toEqual(["a"]);
    expect(find(staff, "SUPPORT").map((x) => x.id)).toEqual(["a", "b"]);
    expect(find(staff, "asha ito")).toEqual([]);
    expect(find(staff, "  ").length).toBe(2);
  });

  it("makes initials for a card", () => {
    expect([marks("Asha Rao"), marks("Madonna"), marks("  ana  maria  de souza "), marks(null)]).toEqual(["AR", "M", "AS", "?"]);
  });
});
