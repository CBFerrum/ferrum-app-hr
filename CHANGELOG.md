# Changelog

## 0.5.0

- Approvals: the leave, attendance and expense requests routed to you wait on one page (the second tab of People), the oldest first. Each shows why it was asked for and what a decision needs beside it: what the leave leaves in the balance and who else in the team is away then; what the days hold now and the punches on them; what was claimed and what is sanctioned, which you can lower line by line. Approve in one click, or reject with a note that goes to the request's discussion; tick several to approve them together; J, K, A and R work from the keyboard. A card leaves at once and comes back, with the server's reason, if it is refused. The same requests are in Home's My work.
- My space (the first tab, and People's home): checking in; your leave as meters (spent, waiting for approval, left); your next time off and the holidays; your leave requests, attendance requests and expense claims in one list; the month's attendance, one disc to a day, with a way to ask for a day to be put right; your payslips; the documents on your record and the training you have passed and until when. It is one column on a phone.
- Team calendar: who is away, by week or month, with public holidays and weekly offs, for your own team (you, your manager, those under them and your own reports) or, for those who see every employee, everyone. Leave types and requests not yet decided are shown only to those who read all leave.
- People: a directory to search and an org chart to open a level at a time (a loop of managers is cut, not drawn twice), with the keyboard walking it as a tree. Phone numbers, the month's joiners and the anniversaries are for those who see every employee; HR also finds the address of the careers page there.
- Careers: a page anyone can open without signing in, at /people/careers/{workspace}: the openings that are published, open and not past their closing date, and an application form (name, email, phone, a link to a CV, a few words). The same address applies to an opening once; a form filled in by a program is thanked and forgotten. Files cannot be uploaded from the public page.
- Attendance requests: an employee asks for a missed check-in to be put right, or for days at home or on duty (up to 31 days, times for a single day). The approver (the employee's leave approver, else their manager) decides through a workflow of its own; approval marks each working day in place of what it had, and cancelling takes the marks off. Days of approved leave, holidays, days to come and days already asked about are refused. Approvers and employees are told, as with leave.
- Leave accrual: a line of a leave policy can grant a share each month, quarter or half-year instead of all at once. Employees name the policy they have, a policy says in which month its leave year starts, and every day the upkeep opens the year's allocation and tops it up to what has accrued, counting what the ledger holds first, so that running it again changes nothing. An allocation made by hand is left alone. Grant Accrued Leaves on the policy does the same on demand.
- Upkeep for one workspace is a function of its own (`upkeep`).

## 0.4.0

- Training: programs, sessions, and what each person has passed and until when.
- Personal data: a person's request now exports their salary slips, tax declarations and leave encashments, and says why they are kept: they are payroll and tax records.
- Personal data: a person's request now reaches job offers, which an erasure blanks, and exports attendance, leave, movements and appraisals, saying why they are kept: they are employment records.

## 0.3.4

- The interface starts faster: its first download holds the frame and the app's own pages, and
  lists, forms, reports and other apps' screens load when first opened (Ferrum
  docs/plan/apps-ui.md §12.9). The build keeps it within the size budget.

## 0.3.3

- Record cards (the bar's previews on hover, Ferrum apps-ui.md U5) stay off your leave requests, the requests waiting for your approval and the directory, whose rows
  already show what a card would.

## 0.3.2

- Rebuilt with Ferrum-Frontend a8d4dda: under the platform's bar the records frame no longer shows
  a second bell (the bar has notifications).

## 0.3.1

- Its notifications have kinds each person chooses for (Ferrum's bell, apps-ui.md U5): leave
  requests and expense claims waiting for your approval, and decisions on yours, in the bar and by
  email unless chosen otherwise. Requires the platform with notification kinds (Ferrum 2fccf10).

## 0.3.0

- People, an interface of its own at /people/ (Ferrum docs/plan/apps-ui.md U5), in `ui/` (built with
  Ferrum-Frontend's packages, `dist` committed and embedded as `UI`). Its home: for the signed-in
  employee, check in and out, leave balances, their leave requests and "Ask for leave"; the leave
  requests waiting for their approval, approved or rejected in place; and, for those who see
  everyone, who is away today, who joined this month, the work anniversaries of the next two
  weeks, and the directory with search. Kept current by live updates. HR's and payroll's other
  records are lists and forms in the records frame with their modules.
- Launcher entry for HR Managers and Users, Employees, Leave Approvers and Payroll Managers; Leave
  request in the Create menu; /open of HR's, payroll's and the hr-erp connector's records.

## 0.2.1

- Reading projects (for expense claims) comes with `hr_erp`, where the platform's shared
  Project is within reach.

## 0.2.0

- HR and payroll work without ERP: `hr` depends on the platform's core and `payroll` on `hr`
  (Ferrum docs/plan/apps-ui.md §13). Payroll runs compute and submit salary slips; they post to
  the ledger where ERP is.
- The connector app `hr-erp` (connectors/erp/app.yaml, shipped with HR, requiring HR and ERP)
  brings what needs ERP: expense claims and employee advances (package `hr_erp`, moved whole from
  `hr`, with their workflow, menu and roles) and payroll's accounts, cost centers, accrual journals
  and bank entries (package `payroll_erp`). Fields keep their names and columns. Requires the
  platform 0.3.2.
- Declares where it keeps people's personal data, so Administration → Personal data exports and
  erases it for a person (by email).

## 0.1.0

- First release as a standalone Ferrum app (previously part of the Ferrum repository).
