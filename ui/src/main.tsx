// People, HR and Payroll's own interface (Ferrum docs/plan/apps-ui.md §13, U5) at /people/: a
// person's own space on its home, the requests waiting for them at /approvals, the team calendar at
// /calendar, the directory and org chart at /directory, rosters at /roster, and the careers page anyone
// may open at /careers/{workspace}; HR's and payroll's records (employees, attendance, leave,
// recruitment, appraisals, salary structures, payroll runs) as lists and forms in the records frame,
// with its modules in the sidebar under the platform's bar. The route table is in routes.tsx.

import { mountApp, setFrameMode } from "@ferrum/records/frame";
import { routes } from "./routes";
import "./styles.css";

/** HR's and payroll's packages: the sidebar shows their modules. */
const PEOPLE_PACKAGES = ["hr", "payroll", "hr_erp", "payroll_erp"];

setFrameMode("app", PEOPLE_PACKAGES);

mountApp({
  route: "people",
  catalogs: { ar: () => import("./locales/ar.json"), hi: () => import("./locales/hi.json") },
  routes,
});
