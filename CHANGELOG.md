# Changelog

## 0.8.3

Your last check-in or out is on the workspace's clock. Rebuilt with Ferrum-Frontend 24c3951, so its records' pages show times on the workspace's clock too, a draft numbered on submit is headed as a draft, and a record of an app that is not installed says so.

## 0.8.2

Rebuilt with Ferrum-Frontend bb60115: a record page reads an empty number or check as the server does, so an action whose condition needs one shows, and a workspace's settings open as their one record.

## 0.8.1

The actions ask for what they read. None declared its parameters, so a page could never give them: Allocate Leaves, which needs the period's dates, could not work from one, and Add Weekly Offs could not without the list's weekly off. They now declare them: Allocate Leaves its dates, company and carry forward, Grant Accrued Leaves its date, Add Weekly Offs its weekday, Mark Attendance Up To Date its last day, Create Employee the date of joining and the member who signs in as them, and the payments of claims and advances (and an advance's return) their amount and bank account (hr-erp connector 0.2.1). Carry forward takes the page's Yes as well as true. A new advance starts with 0 paid, claimed and returned rather than nothing, so its page offers Make Payment Entry: its condition (`paid_amount < advance_amount`) held on the server, which reads an empty number as 0, but not on the page (found walking hire to retire). Create Employee opens the new employee and says who joins when; a claim's or an advance's Make Payment Entry says the draft journal entry waits for accounts in ERP.

## 0.8.0

A leaver's full and final statement (hr-erp connector 0.2.0, package hr_erp 0.2.0): a submitted separation's Create Full and Final Statement makes it with what is open each way, their approved expense claims not yet paid and their paid advances not yet claimed or returned, and gratuity, notice pay or a recovery can be added by hand. Submitted, its Make Payment Entry makes one journal entry that debits what is paid, credits what is recovered and pays the difference from the bank (or takes it in); submitting the entry pays each claim and returns each advance the statement names and marks it paid, and cancelling the entry undoes all of it. One statement per separation; a paid statement is not cancelled before its entry. Expense claims, advances and statements are now declared as personal data kept as accounting records. Rebuilt with Ferrum-Frontend 0f1a544: a settings page is headed by what it is.

## 0.7.4

Rebuilt with Ferrum-Frontend 00dea07: in a workflow state that names what may change, the other fields show read-only, and the fields the state needs show required.

## 0.7.3

Personal-data requests find employee, job applicant records by the person's phone number as well as their email, and list the employee, job applicant records holding their name apart, for an administrator to judge (never erased for the name alone) (Ferrum's FERRUM-PRIVACY-002). Bank account numbers, IBANs, tax and national ID and passport numbers are left out of every list export, whoever exports (FERRUM-IDENTITY-012).

## 0.7.2

Rebuilt with Ferrum-Frontend daeadda: after an action runs, a record page shows what the action said it did rather than only that it is done.

## 0.7.1

- Employees' bank account numbers, IBANs, tax IDs and national IDs show their last four
  characters, and passport numbers that there is one, to those who may not read bank or identity
  details (FERRUM-IDENTITY-012): HR users see "•••• 3344" where they saw nothing. Lists and filters
  keep to what each person may read, as before.
- Interface rebuilt with Ferrum-Frontend a110af4: an action says what it did ("2 payments of
  173247.14 INR in all") rather than "done".

## 0.7.0

A payroll run's **Bank Payment File** (FERRUM-HR-002): the file a bank pays the run from.

- ISO 20022's customer credit transfer initiation, `pain.001.001.09`, with the category purpose
  `SALA`: one payment information block from the company's account, one transaction per employee
  paid by bank, each with its salary slip as the end-to-end reference. An employee with an IBAN is
  paid to it; one without is paid to their account number at the bank their routing code names,
  in the clearing system of the company's country (IFSC in India, ABA in the United States, sort
  codes in the United Kingdom and so on). Euros between IBANs go by SEPA's service level.
- Or a CSV for a bank's bulk-upload sheet: name, account number, IBAN, routing code, bank, amount,
  currency, the day and the reference.
- The company's account is Finance's Bank Account for the account salaries are paid from (the
  run's, else the company's default bank account), or the IBAN or account number given with the
  request where Finance is not installed.
- Read with the caller's own rights: refused to anyone who may not read employees' bank details or,
  from Finance, the company's own; refused, naming them, while anyone paid by bank has no account
  to pay into. Employees paid in cash or by cheque are left out, as is a slip with nothing to pay.

Not yet: validated against the published schema or a bank's own validator (the elements follow
pain.001.001.09's order; banks each add rules of their own), and bank-specific CSV layouts.

## 0.6.1

Rebuilt with Ferrum-Frontend 80ff179: a field's label names its control for assistive technology, and a record whose title is a link shows that record's title, not its id.

## 0.6.0

Income tax as a jurisdiction sets it, beyond progressive slabs.

- An Income Tax Slab takes a rebate: up to its most, while the taxable income is at most its
  limit (India's section 87A: up to 60,000 while it is at most 12 lakh).
- And other taxes and charges: each a percent of the tax so far, where the taxable income is
  within its bounds, in their order, so a surcharge comes before the cess on tax and surcharge.
- `tests/fixtures/india-new-regime-fy2025-26.json` holds India's new regime for FY 2025-26 as the
  Income Tax Department's page prints it, read on 4 November 2026, with the tax at nine incomes
  worked from the page rather than from the code; a unit test checks the engine against it.
  Marginal relief just above the rebate's limit and the surcharge are not in it: the page does not
  state the first, and the second was not read.

## 0.5.1

Hire to retire, to the end (FERRUM-JOURNEY-003).

- An Employee Separation is submitted now. Submitting it needs its relieving date and makes the
  employee one who leaves on it (status Left, that relieving date); cancelling it makes them Active
  again. HR managers submit and cancel separations.
- On the relieving date, in the workspace's time zone, HR's upkeep ends the leaver's sign-in through
  Ferrum's deprovisioning: they cannot sign in again, their sessions, keys and grants end, and what
  they were named on moves to the user of the employee they reported to. It happens once; a refusal
  (the workspace's last administrator) is logged and tried again. Signing them in again later is an
  administrator's.

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
