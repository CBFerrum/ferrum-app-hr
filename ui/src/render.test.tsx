// People's screens drawn once, on the server's side of React, from data the page would have fetched:
// they must draw without failing, say what they should, and keep their words for people. (What
// happens when someone clicks is the server's, which has its own tests; here is only what shows.)

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToString } from "react-dom/server";
import { Route, Routes, StaticRouter } from "react-router";
import { describe, expect, it } from "vitest";
import { Inbox } from "./Approvals";
import { People } from "./Directory";
import { Fix } from "./Fix";
import { PeopleHome } from "./Home";
import { Month } from "./Month";
import { Careers } from "./Openings";
import { TeamCalendar } from "./Team";
import type { Item } from "./inbox";

const ME = { tenant: "t", user: { id: "u1", email: "boss@acme.test", full_name: "Maya Boss", is_admin: false, roles: ["Employee", "Leave Approver"] }, default_company: null, scopes: {}, fiscal_year_start_month: 4, timezone: "Asia/Kolkata", packages: [] };

const perm = (read: "none" | "own" | "all", write: "none" | "own" | "all" = "none") => ({ read, write, create: write, delete: "none", submit: "none", cancel: "none", export: "none", import: "none" });
const META = {
  version: 1,
  packages: ["hr"],
  modules: [],
  warnings: [],
  entities: ["Employee", "LeaveApplication", "AttendanceRequest", "ExpenseClaim", "SalarySlip", "TrainingRecord"].map((name) => ({ name, label: name, label_plural: name, kind: "document", submittable: true, permissions: perm("own") })),
};

function draw(ui: React.ReactElement, location: string, seed: (qc: QueryClient) => void = () => {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["me"], ME);
  qc.setQueryData(["meta"], META);
  seed(qc);
  // React separates the text nodes it draws side by side with a comment; people never see it.
  return renderToString(
    <QueryClientProvider client={qc}>
      <StaticRouter location={location}>{ui}</StaticRouter>
    </QueryClientProvider>,
  ).replace(/<!-- -->/g, "");
}

const who = { id: "e1", name: "Esha Test", department: "Support" };
const ITEMS: Item[] = [
  {
    kind: "leave",
    entity: "LeaveApplication",
    id: "l1",
    version: 2,
    employee: who,
    reason: "Family wedding",
    waiting_since: "2026-10-10T08:00:00Z",
    summary: { leave_type: "Casual Leave", from_date: "2026-10-12", to_date: "2026-10-14", half_day: false, days: "3" },
    context: { balance_before: "10", balance_after: "7", team: { label: "Support", size: 9, away: [{ id: "o", name: "Omar Test", from_date: "2026-10-13", to_date: "2026-10-13" }], away_count: 1, asking_count: 0 } },
  },
  {
    kind: "attendance",
    entity: "AttendanceRequest",
    id: "a1",
    version: 1,
    employee: who,
    reason: "Left without punching out",
    waiting_since: "2026-10-11T08:00:00Z",
    summary: { reason: "Missed Check-in", from_date: "2026-10-06", to_date: "2026-10-06", half_day: false, in_time: "09:05", out_time: "18:10" },
    context: { recorded: [{ date: "2026-10-06", status: "Absent" }], punches: [] },
  },
  {
    kind: "expense",
    entity: "ExpenseClaim",
    id: "x1",
    version: 1,
    employee: { id: "e2", name: "Ravi Test", department: null },
    reason: "Client visit",
    waiting_since: "2026-10-12T08:00:00Z",
    summary: {
      currency: "INR",
      precision: 2,
      claimed: "6500",
      sanctioned: "6500",
      taxes: "0",
      advance: "5000",
      total: "6500",
      lines: [
        { id: "r1", date: "2026-10-10", type: "Travel", description: "Flight", amount: "5000", sanctioned: "5000" },
        { id: "r2", date: "2026-10-11", type: "Food", description: null, amount: "1500", sanctioned: "1500" },
      ],
    },
    context: null,
  },
];

describe("Approvals", () => {
  it("lists what waits, each with its reason and what a decision needs", () => {
    const html = draw(<Inbox />, "/approvals", (qc) => qc.setQueryData(["people", "inbox"], ITEMS));
    for (const word of ["Esha Test", "Ravi Test", "Casual Leave", "Family wedding", "7 days left once it is granted", "1 of 9 in Support is off on these days: Omar", "Left without punching out"]) expect(html, word).toContain(word);
    for (const word of ["Missed check-in", "Absent on", "Says: came in at 09:05, left at 18:10", "Client visit", "Flight", "Sanction", "5,000.00", "set off against an advance"]) expect(html, word).toContain(word);
    expect(html).toContain("Approve Esha Test&#x27;s request");
    expect(html).toContain("Reject Ravi Test&#x27;s claim");
    expect(html).toContain("3 waiting for you, the oldest first");
    for (const word of ["All (3)", "Leave (1)", "Attendance (1)", "Expenses (1)"]) expect(html, word).toContain(word);
    expect(html).toContain("J and K move between requests");
  });

  it("filters by kind from the address", () => {
    const html = draw(<Inbox />, "/approvals?kind=expense", (qc) => qc.setQueryData(["people", "inbox"], ITEMS));
    expect(html).toContain("Ravi Test");
    expect(html).not.toContain("Esha Test</strong>");
  });

  it("says so when nothing waits", () => {
    const html = draw(<Inbox />, "/approvals", (qc) => qc.setQueryData(["people", "inbox"], []));
    expect(html).toContain("Nothing is waiting for you");
    expect(html).toContain("You are up to date");
  });

  it("shows a spinner while it loads", () => {
    expect(draw(<Inbox />, "/approvals")).toContain("Loading what is waiting for you");
  });
});

describe("The team calendar", () => {
  const cal = {
    from: "2026-09-28",
    to: "2026-11-01",
    scope: "team",
    everyone_allowed: true,
    size: 4,
    has_record: true,
    away: [
      { employee: "o", name: "Omar Test", department: "Support", from_date: "2026-10-13", to_date: "2026-10-14", half_day_date: null, pending: false, leave_type: "Casual Leave" },
      { employee: "e", name: "Esha Test", department: "Support", from_date: "2026-10-14", to_date: "2026-10-14", half_day_date: null, pending: true, leave_type: "Casual Leave" },
    ],
    holidays: [
      { date: "2026-10-02", name: "Gandhi Jayanti", weekly_off: false },
      { date: "2026-10-03", name: "Saturday", weekly_off: true },
    ],
  };
  const seeded = (week: boolean) => (qc: QueryClient) => qc.setQueryData(["people", "calendar", week ? "2026-10-12" : "2026-09-28", week ? "2026-10-18" : "2026-11-01", false], cal);

  it("draws a month with its holidays and who is away", () => {
    const html = draw(<TeamCalendar />, "/calendar?date=2026-10-14", seeded(false));
    for (const word of ["October 2026", "Your team, 4 people", "Gandhi Jayanti", "Omar Test", "Everyone", "My team", "Month", "Week", "Holiday", "Weekly off", "Asked for, not decided"]) expect(html, word).toContain(word);
    expect(html).toMatch(/Wednesday,? (14 October|October 14)/);
    expect(html).toContain("holiday: Gandhi Jayanti");
    expect(html).toContain("1 away");
    expect(html).toContain("Asked for");
  });

  it("draws a week as a row for each person who is away", () => {
    const html = draw(<TeamCalendar />, "/calendar?view=week&date=2026-10-14", seeded(true));
    expect(html).toMatch(/(12\s?[–-]\s?18 Oct|Oct 12\s?[–-]\s?18) 2026/);
    for (const word of ["Omar Test", "Esha Test", "Away this week", "Casual Leave"]) expect(html, word).toContain(word);
    expect(html).toContain("ppl-bar");
  });

  it("offers a team to someone with no record, and everyone if they may", () => {
    const html = draw(<TeamCalendar />, "/calendar?date=2026-10-14", (qc) => qc.setQueryData(["people", "calendar", "2026-09-28", "2026-11-01", false], { ...cal, has_record: false, away: [], size: 0 }));
    expect(html).toContain("There is no team to show yet");
    expect(html).toContain("Show everyone");
  });
});

describe("People", () => {
  const people = [
    { id: "a", name: "Asha Rao", designation: "Director", department: "Management", manager: null, email: "asha@acme.test" },
    { id: "b", name: "Ben Ito", designation: "Support Lead", department: "Support", manager: "a", email: "ben@acme.test", phone: "+919845012345", joined: "2026-10-20" },
    { id: "c", name: "Cy Lund", designation: "Engineer", department: "Support", manager: "b" },
  ];
  const dir = (detail: boolean) => (qc: QueryClient) => qc.setQueryData(["people", "directory"], { people, truncated: false, detail });

  it("lists everyone, with a way to search", () => {
    const html = draw(<People />, "/directory", dir(false));
    for (const word of ["Asha Rao", "Ben Ito", "Cy Lund", "3 people", "Directory", "Org chart", "Search by name, role or department", "All departments", "asha@acme.test"]) expect(html, word).toContain(word);
    expect(html).not.toContain("+919845012345");
    expect(html).not.toContain("Joined this month");
  });

  it("gives phones and the month's joiners to those who see every employee", () => {
    const html = draw(<People />, "/directory", dir(true));
    expect(html).toContain("tel:+919845012345");
    expect(html).toContain("Joined this month");
    expect(html).toContain("Work anniversaries");
  });

  it("draws the org chart from the top, closed, with how many report to each", () => {
    const html = draw(<People />, "/directory?view=chart", dir(false));
    expect(html).toContain('role="tree"');
    expect(html).toContain("Asha Rao");
    expect(html).not.toContain("Cy Lund");
    expect(html).toContain("Show the 1 person who report to Asha Rao");
    expect(html).toContain("Open every line");
  });
});

describe("My space", () => {
  const mine = { id: "e1", employee_name: "Esha Test", _titles: { designation: "Engineer", department: "Support" } };
  const space = (qc: QueryClient) => {
    qc.setQueryData(["people", "me", "boss@acme.test"], mine);
    qc.setQueryData(["people", "inbox"], ITEMS);
    qc.setQueryData(["people", "balance", "e1"], [{ leave_type: "Casual Leave", allocated: "12", taken: "2", booked: "1", pending: "3", balance: "10", available: "6" }]);
  };

  it("says who it is for, what is waiting, and what is left of their leave", () => {
    const html = draw(<PeopleHome />, "/", space);
    for (const word of ["My space", "Esha Test · Engineer · Support", "3 requests are waiting for your decision", "Your leave", "6 days", "Casual Leave", "of 12 days", "3 days waiting for approval", "Coming up", "Your requests", "Ask for leave"]) expect(html, word).toContain(word);
    expect(html).toContain("Casual Leave: 12 days in all, 3 days waiting for approval, 6 days left");
  });

  it("explains a sign-in that is not linked to an employee", () => {
    const html = draw(<PeopleHome />, "/", (qc) => qc.setQueryData(["people", "me", "boss@acme.test"], null));
    expect(html).toContain("Your sign-in is not linked to an employee record");
  });

  it("draws the month's attendance with a mark for each day", () => {
    const html = draw(
      <Month employee={{ id: "e1" }} onFix={() => {}} />,
      "/?month=2026-10",
      (qc) =>
        qc.setQueryData(["people", "month", "e1", "2026-10-01"], {
          attended: [
            { attendance_date: "2026-10-01", status: "Present", in_time: "2026-10-01T03:40:00Z", out_time: "2026-10-01T12:40:00Z", working_hours: "9" },
            { attendance_date: "2026-10-05", status: "Absent" },
          ],
          holidays: [{ date: "2026-10-02", name: "Gandhi Jayanti", weekly_off: false }],
        }),
    );
    for (const word of ["Your attendance", "October 2026", "Earlier month", "Later month", "1 present", "1 absent"]) expect(html, word).toContain(word);
    expect(html).toMatch(/Thursday,? (1 October|October 1): Present/);
    expect(html).toMatch(/Monday,? (5 October|October 5): Absent/);
    expect(html).toMatch(/Friday,? (2 October|October 2): Gandhi Jayanti/);
  });

  it("asks for attendance to be put right, in words a person would say", () => {
    const html = draw(<Fix employee="e1" day="2026-10-06" onClose={() => {}} />, "/");
    for (const word of ["Ask for attendance to be put right", "I forgot to check in or out", "I worked from home", "I was on duty away from the office", "Came in at", "Left at", "More than one day", "Tell your approver more", "Send for approval"]) expect(html, word).toContain(word);
    expect(html).toContain('value="2026-10-06"');
  });
});

describe("The careers page", () => {
  const route = () => (
    <Routes>
      <Route path="careers/:slug" element={<Careers />} />
      <Route path="careers/:slug/:opening" element={<Careers />} />
    </Routes>
  );

  it("lists the openings", () => {
    const html = draw(route(), "/careers/acme", (qc) =>
      qc.setQueryData(["careers", "acme"], { openings: [{ id: "o1", title: "Support Engineer", company: "Acme Pvt Ltd", department: "Support", branch: "Chennai", employment_type: "Full-time", posted_on: "2026-10-20", closes_on: null, description: null, designation: "Engineer" }] }),
    );
    for (const word of ["Careers", "Support Engineer", "Acme Pvt Ltd · Support · Chennai · Full-time"]) expect(html, word).toContain(word);
    expect(html).toContain('href="/careers/acme/o1"');
  });

  it("says so when there is nothing to apply for", () => {
    expect(draw(route(), "/careers/acme", (qc) => qc.setQueryData(["careers", "acme"], { openings: [] }))).toContain("No openings right now");
  });

  it("shows an opening with its description and the form to apply", () => {
    const html = draw(route(), "/careers/acme/o1", (qc) =>
      qc.setQueryData(["careers", "acme", "o1"], { id: "o1", title: "Support Engineer", company: "Acme Pvt Ltd", department: "Support", branch: null, employment_type: null, posted_on: null, closes_on: "2026-12-01", description: "Help **people** with printers.", designation: null }),
    );
    for (const word of ["Support Engineer", "Help", "printers", "Apply for this role", "Your name", "Email address", "Link to your CV or profile", "A few words about you", "Send my application", "We use what you send only to consider you", "All openings"]) expect(html, word).toContain(word);
    expect(html).toContain('tabindex="-1"');
  });
});
