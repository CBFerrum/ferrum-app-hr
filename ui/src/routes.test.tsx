// The route table People mounts, opened at its roster pages: each address matches its route, and the route's page
// loads and is a component.

import { createMemoryRouter, type RouteObject } from "react-router";
import { describe, expect, it } from "vitest";
import { routes } from "./routes";

describe("People's routes", () => {
  it("open the roster list and a roster's grid", async () => {
    const children = routes.flatMap((r) => r.children ?? []);
    for (const [at, path] of [
      ["/roster", "roster"],
      ["/roster/r1", "roster/:id"],
    ]) {
      const router = createMemoryRouter(routes, { initialEntries: ["/"] });
      await router.navigate(at);
      expect(router.state.matches.at(-1)?.route.path, at).toBe(path);
      const route = children.find((r) => r.path === path) as RouteObject;
      const page = await (route.lazy as () => Promise<{ Component?: unknown }>)();
      expect(typeof page.Component, at).toBe("function");
    }
  });
});
