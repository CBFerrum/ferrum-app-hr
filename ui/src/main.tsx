// People, HR and Payroll's own interface (Ferrum docs/plan/apps-ui.md §13, U5) at /people/: a
// person's own space on its home, the requests waiting for them at /approvals, the team calendar at
// /calendar, the directory and org chart at /directory, and the careers page anyone may open at
// /careers/{workspace}; HR's and payroll's records (employees, attendance, leave, recruitment,
// appraisals, salary structures, payroll runs) as lists and forms in the records frame, with its
// modules in the sidebar under the platform's bar.

import { useEffect } from "react";
import { Link, Outlet, useRouteError } from "react-router";
import { shell, t } from "@ferrum/client";
import { EmptyState, ErrorState } from "@ferrum/kit";
import { mountApp, RequireAuth, setFrameMode } from "@ferrum/records/frame";
import { PeopleHome } from "./Home";
import "./styles.css";

/** One of People's own pages, loaded when first opened (the home is the only one every visit needs). */
const own = <K extends string>(load: () => Promise<Record<K, React.ComponentType>>, name: K) => () => load().then((m) => ({ Component: m[name] }));

/** A page of the records frame, loaded when first opened. */
const page = (name: "FormView" | "ListView" | "ReportView") => () =>
  import("./records").then((m) => ({ Component: m[name] }));

/** HR's and payroll's packages: the sidebar shows their modules. */
const PEOPLE_PACKAGES = ["hr", "payroll", "hr_erp", "payroll_erp"];

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

setFrameMode("app", PEOPLE_PACKAGES);

mountApp({
  route: "people",
  catalogs: { ar: () => import("./locales/ar.json"), hi: () => import("./locales/hi.json") },
  routes: [
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
        { path: "e/:entity", lazy: page("ListView") },
        { path: "e/:entity/:id", lazy: page("FormView") },
        { path: "r/:name", lazy: page("ReportView") },
        { path: "*", element: <NotFound /> },
      ],
    },
  ],
});
