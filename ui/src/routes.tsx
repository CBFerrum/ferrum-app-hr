// People's route table (main.tsx mounts it): its own pages, the careers page anyone may open, and the records
// frame's lists, forms and reports. A module of its own so that a test can open its routes.

import { useEffect } from "react";
import { Link, Outlet, type RouteObject, useRouteError } from "react-router";
import { shell, t } from "@ferrum/client";
import { EmptyState, ErrorState } from "@ferrum/kit";
import { RequireAuth } from "@ferrum/records/frame";
import { PeopleHome } from "./Home";

/** One of People's own pages, loaded when first opened (the home is the only one every visit needs). */
const own = <K extends string>(load: () => Promise<Record<K, React.ComponentType>>, name: K) => () => load().then((m) => ({ Component: m[name] }));

/** A page of the records frame, loaded when first opened. */
const page = (name: "FormView" | "ListView" | "ReportView") => () =>
  import("./records").then((m) => ({ Component: m[name] }));

function RouteError() {
  return <ErrorState error={useRouteError()} onRetry={() => window.location.reload()} />;
}

function NotFound() {
  return (
    <EmptyState title={t("Page not found")}>
      <Link to="/">{t("Go to People's home")}</Link>
    </EmptyState>
  );
}

/** Every page tells the shell it started. */
function Started() {
  useEffect(() => shell().ready(), []);
  return null;
}

/** A page anyone may open: no sign-in, no bar. */
function Public() {
  return (
    <>
      <Started />
      <Outlet />
    </>
  );
}

export const routes: RouteObject[] = [
  // Open to anyone, so outside the sign-in.
  {
    path: "careers/:slug",
    element: <Public />,
    errorElement: <RouteError />,
    children: [
      { index: true, lazy: own(() => import("./Openings"), "Careers") },
      { path: ":opening", lazy: own(() => import("./Openings"), "Careers") },
    ],
  },
  {
    path: "/",
    element: (
      <>
        <Started />
        <RequireAuth />
      </>
    ),
    errorElement: <RouteError />,
    children: [
      { index: true, element: <PeopleHome /> },
      { path: "approvals", lazy: own(() => import("./Approvals"), "Inbox") },
      { path: "calendar", lazy: own(() => import("./Team"), "TeamCalendar") },
      { path: "directory", lazy: own(() => import("./Directory"), "People") },
      { path: "roster", lazy: own(() => import("./Rosters"), "Rosters") },
      { path: "roster/:id", lazy: own(() => import("./Rosters"), "RosterGrid") },
      { path: "e/:entity", lazy: page("ListView") },
      { path: "e/:entity/:id", lazy: page("FormView") },
      { path: "r/:name", lazy: page("ReportView") },
      { path: "*", element: <NotFound /> },
    ],
  },
];
