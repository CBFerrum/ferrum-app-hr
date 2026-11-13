<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/assets/banner-dark.svg">
  <img alt="Ferrum People: HR and payroll from hire to final settlement" src="docs/assets/banner-light.svg" width="100%">
</picture>

<br>

[![Version](https://img.shields.io/badge/version-0.14.0-0e7490?style=flat-square)](CHANGELOG.md)
[![Licence: AGPL-3.0](https://img.shields.io/badge/licence-AGPL--3.0-blue?style=flat-square)](LICENSE)
[![Ferrum platform](https://img.shields.io/badge/runs%20on-Ferrum-2f5bea?style=flat-square)](https://github.com/CBFerrum/Ferrum)
![Rust](https://img.shields.io/badge/Rust-server-b7410e?style=flat-square&logo=rust)
![TypeScript](https://img.shields.io/badge/React-TypeScript-3178c6?style=flat-square&logo=typescript&logoColor=white)
![PostgreSQL](https://img.shields.io/badge/PostgreSQL-data-336791?style=flat-square&logo=postgresql&logoColor=white)
![Tests](https://img.shields.io/badge/tests-57%20Rust%20%C2%B7%2055%20UI-16a34a?style=flat-square)

### Run people and payroll in one place: hire, onboard, track time and leave, pay by formula, and settle on the way out.

People (HR and Payroll) is the employee lifecycle for Ferrum: employees and organisation, shifts and attendance, leave with a ledger, recruitment, onboarding and offboarding, letters, appraisals, expense claims, and a payroll that computes salary slips from formulas, posts one balanced accrual to the ledger and pays by bank file. It is for companies that want HR and payroll in the same system as their books, and it replaces a stand-alone HR tool plus a payroll spreadsheet.

[💡 Why](#-why-people) · [📸 Screenshots](#-screenshots) · [✨ Features](#-features) · [📊 Compare](#-how-it-compares) · [📍 Roadmap](#-roadmap) · [🚀 Install](#-install) · [🔧 Develop](#-develop)

</div>

## 💡 Why People

- 🧾 **Payroll posts to your books.** A run creates the slips, posts one balanced accrual journal for the whole run and pays through a bank entry and a bank file. No export to accounting.
- 🧮 **Pay rules are formulas, not code.** Components are written in CEL over the employee, the work location, the period, year-to-date figures, declarations and lookup tables. Nothing in the app names a country: a country's rules are data.
- 🔁 **Mistakes can be put right.** Correct one slip after the run is posted, pay arrears for a backdated revision, run off-cycle, hold and release a salary, carry late leave to the next pay. Nothing is lost and nothing is paid twice.
- 🚪 **From offer to final settlement.** An accepted offer becomes an employee with an onboarding checklist. A separation with notice ends in one full and final statement that gathers pay, leave, notice, gratuity, claims and advances into one journal entry.
- 🙋 **Self-service that respects who you are.** Employees check in, ask for leave, fix attendance, claim expenses, read payslips and documents. Managers decide from one inbox. Pay, bank and identity details are held back from those who may not read them.
- 🌍 **Arabic and Hindi built in**, right-to-left pages, and a workspace's own interface words and letter wording.
- 🔓 **Open source and self-hosted.** AGPL-3.0, no per-employee price, your data in your PostgreSQL, open to Ferrum's REST and MCP APIs under each person's permissions.

## 🎯 At a glance

| | |
|---|---|
| **Version** | 0.14.0 (packages `hr` 0.6.0, `payroll` 0.7.0, `hr_erp` 0.5.0, `payroll_erp` 0.4.0) |
| **Kind** | Its own interface at `/people/` (your space, approvals, team calendar, directory and org chart, public careers page) with Ferrum's records screens for everything else |
| **What it adds** | 85 entities (56 records and 29 small tables), 7 roles, 6 modules (HR, Attendance and Leave, Recruitment, Performance, Expenses, Payroll), 3 approval workflows and 2 package approval policies, 12 reports, 1 print format, 7 document templates, 5 message templates, 6 notification kinds, 7 upkeep jobs, 2 example packs |
| **Depends on** | Ferrum platform `>=0.3.2, <0.4`. The connector **HR and ERP** (expenses and payroll posting) needs ERP `>=0.1` and ships with this app |
| **Works with** | ERP (ledger, accounts, cost centers, company letterhead), Finance (bank accounts), Projects and Objectives (people), Helpdesk and others through the shared Employee record |
| **Languages** | English, Arabic and Hindi (253 interface words and 1,190 names in each), right-to-left |
| **Tests** | 57 Rust tests (against a real PostgreSQL), 55 interface tests |
| **Licence** | AGPL-3.0-only |

## 📸 Screenshots

<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-home-dark.webp"><img alt="People — home" src="docs/assets/screens/people-home-light.webp" width="100%"></picture>

<p align="center"><b>🏠 My space</b><br><sub>Check in, see your leave, requests, attendance month, payslips and documents in one place.</sub></p>

<table>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-employee-list-dark.webp"><img alt="Employees — list" src="docs/assets/screens/people-employee-list-light.webp"></picture>
<p><b>🧑‍💼 Employees</b><br><sub>Browse and filter employees by department, designation and status.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-leave-application-list-dark.webp"><img alt="Leave applications — list" src="docs/assets/screens/people-leave-application-list-light.webp"></picture>
<p><b>🌴 Leave applications</b><br><sub>Read leave requests with their dates, days and approval status.</sub></p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-attendance-list-dark.webp"><img alt="Attendance — list" src="docs/assets/screens/people-attendance-list-light.webp"></picture>
<p><b>🕘 Attendance</b><br><sub>See who was present, absent, on leave or working from home, with hours and flags.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-department-list-dark.webp"><img alt="Departments — list" src="docs/assets/screens/people-department-list-light.webp"></picture>
<p><b>🏢 Departments</b><br><sub>Keep the department tree that employees, leave and payroll group by.</sub></p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-employee-detail-dark.webp"><img alt="Employee — open record" src="docs/assets/screens/people-employee-detail-light.webp"></picture>
<p><b>🧾 One employee</b><br><sub>Open an employee for their job, reporting line, leave, bank details and history.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-employee-new-dark.webp"><img alt="Employee — new" src="docs/assets/screens/people-employee-new-light.webp"></picture>
<p><b>➕ New employee</b><br><sub>Fill in the form to add an employee.</sub></p>
</td>
</tr>
<tr>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-report-employee-directory-dark.webp"><img alt="Employee directory — report" src="docs/assets/screens/people-report-employee-directory-light.webp"></picture>
<p><b>📒 Employee directory report</b><br><sub>Run the directory by company, department, designation and status.</sub></p>
</td>
<td width="50%" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-report-headcount-dark.webp"><img alt="Headcount and attrition — report" src="docs/assets/screens/people-report-headcount-light.webp"></picture>
<p><b>📊 Headcount and attrition report</b><br><sub>Count headcount and leavers by department, designation, branch, grade, employment type or gender.</sub></p>
</td>
</tr>
</table>

<table>
<tr>
<td align="center" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-home-phone-dark.webp"><img alt="People — home" src="docs/assets/screens/people-home-phone-light.webp" width="260"></picture>
<p><b>📱 My space (phone)</b><br><sub>Your day on a phone, in one column.</sub></p>
</td>
<td align="center" valign="top">
<picture><source media="(prefers-color-scheme: dark)" srcset="docs/assets/screens/people-employee-list-phone-dark.webp"><img alt="Employees — list" src="docs/assets/screens/people-employee-list-phone-light.webp" width="260"></picture>
<p><b>📱 Employees (phone)</b><br><sub>The employee list on a phone.</sub></p>
</td>
</tr>
</table>


## ✨ Features

### 🏢 Organisation and employees

- **Employee** holds a person employed by a company: names, status (**Active**, **Inactive**, **Suspended**, **Left**), department, designation, grade, branch, employment type, reporting line, leave approver, expense approver, holiday list, default shift, leave policy, attendance device id, work and personal email, phone, addresses, emergency contact, marital status, blood group, joining date, notice period, and salary mode. The linked sign-in user sees the record and their own HR documents.
- **Identity and bank details** (bank name, account, IFSC, IBAN, tax id, national id, passport) sit in field groups: only those who may read them see them, and the rest see the last four characters.
- **Rules that keep it honest:** joining after birth, relieving after joining, a Left employee needs a relieving date, no reporting loop.
- **Departments** (a tree with parents and groups), **Designations**, **Branches** (address, region, country, a point and a radius for check-ins), **Grades** and **Employment Types** (Probation is seeded).
- **Promotions** and **Transfers** are submitted documents. On submit, the employee takes the new designation, grade, department, branch or manager, and the previous values stay on the document.
- **Hand-over.** When a person leaves, their open work moves to their manager and their sign-in ends on the relieving date through Ferrum's deprovisioning.
- **Directory and org chart** (`/people/directory`): everyone's name, role, department, place and work email; phone numbers, joiners, anniversaries and who is serving notice only for those who see every employee. The org chart opens from the people who answer to nobody, one level at a time, with search that jumps to a person and the keyboard walking it as a tree.
- **Reports:** Employee Directory, Headcount and Attrition (by department, designation, branch, grade, employment type or gender) and Upcoming Birthdays and Work Anniversaries.

### 📄 Employee documents

- **Document Types** (General, Identity or Bank category): who sees it (HR only, Employee, Employee and Manager), whether employees upload it, whether it expires and how many days before to warn (30 by default), and whether it is required for everyone or for a department, designation, employment type or branch.
- **Employee Documents** hold the file, number, issue and expiry dates and who verified it. **Verify** is HR's, never the employee's; changing what was verified takes the verification away.
- **Visibility.** The employee sees their own of the types shown to them, their manager a general one whose type says so, and an HR user no bank document.
- **Expiry warnings.** The upkeep warns the employee and the HR managers once per expiry date.
- **Report:** Expiring and Missing Documents.

### 🕘 Shifts, holidays and attendance

- **Holiday Lists** with holidays and weekly offs; leave days and payroll working days skip them.
- **Shift Types**: start and end (overnight allowed), grace periods for late entry and early exit, thresholds for half day and absent, the window to begin check-in before and allow check-out after, how working hours are calculated, and an option to mark absent without check-ins. **Shift Assignments** give an employee a shift for a date range.
- **Check-ins.** One punch (IN or OUT) from a device, the app or HR. Devices may send the employee's attendance device id instead of the employee. A check-in can carry latitude, longitude, accuracy, a photo and a device mark.
- **Place rules.** A branch has a point and a radius (200 m by default) and a rule for its employees' own check-ins outside it or without a position: **Off**, **Warn** or **Refuse**. A shift may say otherwise. Distance and Place are kept on the check-in.
- **Attendance from check-ins.** Every minute, once a shift day is over, attendance is marked in the tenant's time zone: punches inside the shift's window count, working hours decide between **Present**, **Half Day** and **Absent**, and **late entry** and **early exit** are flagged. Statuses are Present, Absent, Half Day, On Leave and Work From Home.
- **Attendance Requests.** A missed check-in put right, or days worked from home or away on duty, approved by the approver (or an HR manager) through a workflow. The server refuses a day to come, a holiday or a day of leave and says why next to the field.
- **Attendance Device role** for the API keys of biometric or RFID devices: they can only post check-ins.
- **Monthly Attendance Sheet** report, one row per employee with a cell per day.

### 🌴 Leave

- **Leave Types:** maximum per year, applicable after, maximum continuous days, leave without pay, include holidays, allow negative balance, carry forward with a cap, encashment with a threshold.
- **Leave Policies** list a type and its annual allocation, granted all at once or by **Monthly**, **Quarterly** or **Half-yearly** accrual. A policy chosen on the employee drives their allocation; the upkeep accrues each share when it is due.
- **Allocations** with carry forward of unused leaves, and expiry of carried leaves by the upkeep.
- **Leave Applications** are approved by the leave approver (or an HR manager) through a workflow: Open, Approved, Rejected, Cancelled. Half days on a chosen date. The balance is shown on the form.
- **A ledger, not a counter.** Every change to a balance is an append-only ledger entry (allocation adds, application, encashment and expiry subtract), tied to the allocation it draws on. A balance is a sum; cancelling posts reversing entries.
- **Encashment** pays unused leave through an additional salary in the next slip, while employed and on leaving.
- **Leave Balance** report.
- **Approvals page** shows what a leave leaves in the balance and who else on the team is away.

### 👥 Team calendar and approvals

- **Team Calendar** (`/people/calendar`): who is away, by week or month, with holidays and weekly offs shaded and named. A month shows how many are away each day and opens a day to the names; a week shows each away person as a row. Your own team's, or everyone's for those whose rights reach everyone; the view, day and scope are in the address.
- **Approvals** (`/people/approvals`): leave, attendance requests and expense claims routed to you, oldest first, each with why it was asked and what a decision needs beside it (what the leave leaves in the balance and who else is away; what the days hold now; what was claimed against what is sanctioned). Approving is one click; rejecting asks for a note, which goes to the request's discussion. The count shows in the navigation and Ferrum Home's My work.

### 🙋 My space

A person's own page at `/people/`:

- **Check in** and out, with the last punch.
- **Your leave** balances by type, and **Coming up** (your time off and the holidays in the next two months).
- **Your requests** of every kind in one list, with the buttons **Ask for leave**, **Fix attendance** and **Claim expenses**.
- **Your attendance** as a month of discs: present, working from home, half day, absent, on leave and days off, each day opening to when you came in and left and the hours, and the way to ask for it to be put right.
- **Your payslips** and a link to your **tax declarations**.
- **Documents and training:** the documents kept on your record and the training you have passed and until when it counts.

On a phone it is one column in the order a day goes.

### 🧳 Recruitment

- **Job Requisitions** ask before they are submitted: the hiring manager's manager when the request names a hiring manager whose manager has a sign-in, otherwise an HR Manager; the requester never approves their own. Submitted, the requisition opens its job. A workspace can switch the approval off or replace it.
- **Job Openings** with designation, department, employment type, branch, vacancies, salary range, interview **criteria with weights**, a publish switch and an application link. Openings close by the upkeep on their date.
- **Recruitment Stages** per job (else the defaults), in order, by category: Screening, Interview, Offer, Hired, Rejected. Applicants start at the first; every move is kept; a Rejected stage needs a reason and rejects them; Hired accepts them.
- **Job Applicants** with source (Campaign, Employee Referral, Walk In, Website Listing, Job Portal, Agency, Other), resume link or file (HR can attach a file; the public form takes a link), cover letter and stage history. One application per address per opening.
- **Interviews** with rounds, interviewers, per-interviewer **scorecards** (a rating per criterion), an average rating and a weighted score, and feedback with a result.
- **Job Offers** are submitted documents: CTC, terms, joining date. They ask for approval first: an HR Manager and, by salary band, the people **Recruitment Settings** names (an offer above Band 1 or Band 2 asks that person too). **Mark Accepted** or **Mark Rejected**; accepted, the **Create Employee** action (which asks who signs in) makes the employee, moves the applicant to Hired and starts their onboarding.
- **Careers page** (`/people/careers/{workspace}`): open to anyone without signing in: the published openings, one opening in full, and an application form with a link to a CV or profile. The server checks every field again.
- **Recruitment Pipeline** report.

### 🚀 Onboarding and offboarding

- **Checklist Templates** (Onboarding or Offboarding) match by department, designation and employment type (each empty for any), the highest priority first. Tasks name a role or an employee field (such as the manager) as who does it, due days from the joining or last working day, a document type to collect, whether required, and whether it needs approval.
- **Onboarding** takes the matching template and copies its tasks in. An accepted offer's employee gets their onboarding. A task with a document type waits for that document; one asking approval counts once someone else approves it; the checklist is complete when every required task is.
- **Separation.** A submitted separation records the notice: the day it was given, the notice due (the employee's, else the company's; none on a death, retirement or contract end), the last working day, the notice served and the shortfall (**recover from the employee**, **pay in lieu** or **waive**), exit interview and an exit checklist. The employee keeps working until the last working day: still Active, paid (the last month prorated to the day), accruing leave and in the directory. The upkeep makes them Left once the day has passed and ends their sign-in. Withdrawn, it puts them back.
- Assignees see their tasks in My work, may change only theirs, and are reminded once when one is overdue.

### 🧾 Full and final statement

One **Full and Final Statement** gathers, from the employee's records, what is owed each way when they leave: the pay earned up to the last working day that no slip covers yet (a salary slip of its own, which settles the year's income tax), encashable leave, notice pay or recovery, gratuity by the company's rule, open expense claims, a held or still owed run salary, unsettled advances and loans, and what a corrected run slip paid too much. Each gathered line keeps what was gathered beside the amount, which a person may change with a reason, and lines can be added. One journal entry settles it all.

**Gratuity** is settings on the company (years of service needed, days' pay per year, days in a month, a pay formula and how part years count). No country's rule is built in; a death waives the minimum.

### 💼 Expenses and advances

- **Expense Claim Types** map to accounts for each company.
- **Expense Claims** have lines (date, type, description, amount, sanctioned amount, cost center, project), taxes and charges, and advances settled. **Get Advances** pulls the employee's open advances. Approved through the **Expense Approval** workflow, a claim posts the sanctioned expenses against the employee payable and any advances it settles.
- **Employee Advances:** money paid ahead of expenses, settled by claims or returned, paid by journal entries that reference the advance (reversible vouchers).
- **Report:** Expense Claim Summary.

### 💸 Payroll

- **Salary Components** are earnings, deductions or **employer contributions** (the employer's share of provident fund, insurance or social security, never part of gross or net pay). Amounts are fixed or **formulas in CEL** with a condition. A formula sees `base`, `variable`, `gross`, `payment_days`, the rows above it by abbreviation, `employee.<field>` and `location.<field>` (a workspace's own fields included), `period` (start, end, month, year, number, count, left, days), `ytd.<abbr>` and year-to-date totals, `previous.<abbr>`, `declared.<code>` and `verified.<code>`, and lookup tables with `lookup()` and `banded()`, `years_between` and `months_between`. Results are exact decimals.
- **Lookup tables** with effective dates, keys and bands: a ceiling by year, a slab by region, bands of a tax.
- **Salary Structures** list earnings first, then deductions, then employer contributions, in row order; a formula may use rows above it. **Salary Structure Assignments** put an employee on a structure with a base and a variable, from a date; the latest submitted one on or before a date is in force. Payroll frequency: Monthly, Fortnightly or Weekly.
- **Payroll Periods and income tax.** The tax year over which income tax is projected and spread. **Income Tax Slabs** are progressive, with a standard exemption, declared exemptions, a **rebate** while taxable income is under a limit, and **cesses and surcharges** on the tax, each within its bounds and in order. India's new regime for FY 2025-26 is checked against the published figures; no country's rules ship as code.
- **Tax declarations.** Exemption categories with a code and a maximum; **Employee Tax Exemption Declarations** with amounts and proofs; **Verify Proofs** by HR.
- **Opening figures.** **Payroll Openings** carry what the system you moved from paid, and what a previous employer paid, for the year, so a mid-year move or a joiner is taxed on the whole year with credit.
- **Salary Slips** are recomputed on every save while draft: working days, payment days (less leave without pay and absences, half days count half), earnings, deductions, employer contributions, additional salary, arrears, loan instalments, taxable earnings, income tax, rounded total. A leaver's last slip settles the year's tax.
- **Payroll Entries (runs):** **Get Employees** by branch, department and designation, **Create Salary Slips**, **Submit Salary Slips** (one balanced accrual for the whole run: earnings debit their expense accounts, deductions credit their liability accounts, employer contributions debit their cost and credit what is owed, each net pay credits payroll payable), then the bank entry. Cancelling reverses the journals and is refused while a payment stands.
- **Additional Salary:** a bonus, arrears, a recovery or leave encashment paid in the slip covering its date; or one that **repeats** (in every slip, or every N-th, until a date) with **Stop or Change**. An off-cycle one pays on its own day.
- **Off-cycle runs** for one day and the employees listed, paying only what is named for them.
- **Arrears.** When a salary assignment takes effect in periods already paid, the next slip works each such period out again as it was paid and pays the difference (or takes back a lower one), with the breakdown on the slip. Never paid twice.
- **Correct one slip** after the run is posted: the slip is cancelled with its share of the accrual taken back, and a draft for the same period is made to put right and submit. If the run was already paid, the correction takes over what the bank paid: a higher one leaves the difference owed, a lower one leaves an excess recovered from the next slip (never more than that pay).
- **Hold and release a salary** with a reason: accrued as everyone's, left out of the bank entry and file, paid with the next payment that lists the employee.
- **Leave and attendance changed after payroll ran** is refused until someone chooses on the slip: **Carry to Next Pay** (a pay adjustment on the next slip at that day's pay as paid) or **Correct the Slip**.
- **Employee loans** (with the ERP connector): paid from the bank, repaid from each monthly slip over a number of months or an amount a month, with interest on the balance at a yearly rate. **Defer an Instalment**, **Repay Outside Salary**, **Write Off**. A slip that cannot take it all leaves the rest to the next; a corrected or cancelled slip gives its instalment back. A leaver's balance is a recovery on the final statement.
- **Bank payment file** from a run: **ISO 20022** salary credit transfer (`pain.001.001.09`, purpose `SALA`) or a **CSV** for a bank's bulk upload, one payment to each employee paid by bank, from the company's account. Refused while anyone paid by bank has no account, and to those who may not read bank details. The bank entry and the file pay the same.
- **A run paid outside Ferrum** can be recorded as paid elsewhere (with a date and a reference), and undone.
- **Reports:** Salary Register, Payroll Summary, Bank Remittance and Income Tax Deductions (with opening figures and a previous employer's).

### ✉️ Letters, certificates, payslips and files

Everything a person receives on paper is a Ferrum document template (Settings, Documents): a workspace edits the wording, sees the default beside it and resets it. Letterheads come from the company. Amounts appear in figures and words in the document's language and the company's currency. Nothing names a country.

| Template | About | Button |
|---|---|---|
| `hr.offer_letter` | a submitted Job Offer | Issue Offer Letter |
| `hr.appointment_letter`, `hr.employment_certificate`, `hr.warning_letter` | an Employee | Issue Appointment Letter, Issue Employment Certificate, Issue Warning Letter (it asks what it is about) |
| `hr.relieving_letter` | a submitted Employee Separation | Issue Relieving Letter |
| `payroll.salary_certificate` | a submitted Salary Structure Assignment | Issue Salary Certificate (HR and payroll managers) |
| `payroll.payslip` | a Salary Slip | Send Payslip |

- A button makes an **Employee Letter** register entry whose number runs in the series set on the Employee Document Type for its letter code. The letter is filed among the employee's documents and mailed.
- **The payslip** also prints from the salary slip's print format: company, period, employee, days, earnings and deductions side by side with arrears, adjustments and loan instalments, employer contributions apart, net pay; Draft, Cancelled, Off-cycle, Final settlement and held salaries are marked. **Send Payslip** keeps the file on the slip, files it once among the employee's documents and mails it; tax id, account number and IBAN show their last four characters only.
- **Files from a payroll run.** A document template about a saved report over slips is a file or a page (text, CSV or XML, one row per employee, any employee field a column), made on demand by those who may export and kept on the audit trail. `packs/` has two generic example packs (install `1-reports`, then `2-templates`): a fixed-width contributions file with a header, a line per employee and a trailer, a CSV of net pay, and an annual statement per employee, mailed to each.
- The bank file (ISO 20022 and CSV) is still written in Rust: its payees are computed from what the bank entry pays, which a template cannot see.

### 📈 Appraisals and training

- **Appraisal Cycles**, **Appraisal Templates** with goals (KRAs) and weightages (which must total 100), and **Appraisals**: the employee scores their goals, HR or the manager scores them too and submits; a final score is worked out.
- **Training Programs** (mandatory or not, valid for months), **Training Events** with attendees whose results are marked before submitting, and **Training Records** of what someone has completed and until when it counts, shown in My space.

### 🔔 Notifications

Leave requests and expense claims waiting for your approval, decisions on them, and attendance requests waiting and decided, each by the channel the person chooses (in-app and email by default). The upkeep also sends an overdue checklist task reminder, document expiry warnings, and letter issued and requested messages.

### 🔐 Roles and permissions

| Role | What it can do |
|---|---|
| **HR Manager** | Everything in HR, including bank and identity details; approves requisitions and offers |
| **HR User** | Day-to-day HR work; no bank details and no approvals |
| **Employee** | Self-service: sees and creates only their own records (the employee whose user is the signed-in user) |
| **Leave Approver** | Approves or rejects the leave applications and attendance requests routed to them |
| **Expense Approver** | Sanctions, approves or rejects the expense claims routed to them |
| **Payroll Manager** | Salary structures, payroll runs and payroll reports, including bank details |
| **Attendance Device** | For the API keys of biometric or RFID devices; can only post check-ins |

Records about an employee carry the employee's sign-in email, so employees see their own leave, attendance, claims and slips whoever created them. Pay and bank fields are in permission groups. Employees' personal data is included in a person's export or erasure request, by email and by phone number; accounting records are kept.

### 🔌 For other apps and agents

- Every entity has Ferrum's REST and MCP API under each person's permissions, search, an audit trail, exports and live updates.
- A short assistant note tells Ferrum's assistant what the records mean (states, balances from the ledger, the recruitment path) and suggests "Who is on leave this week?", "How many leave days do I have left?", "Which openings have the most applicants?" and "Who joined this quarter?". Pay, bank, identity and personal details are held back from it unless the workspace allows them.
- `GET /api/app/hr/work` feeds Ferrum Home's My work with the requests that wait for your decision.

## 🧭 How it works

```mermaid
flowchart LR
    SC["Salary components<br/>formulas in CEL"] --> SS["Salary structure<br/>and assignment"]
    SS --> PE["Payroll entry<br/>period, branch, department"]
    PE -->|"creates"| SL["Salary slips"]
    SL -->|"submit"| AC["One balanced accrual<br/>journal entry"]
    AC -->|"pay"| BK["Bank entry and bank file<br/>held salaries left out"]
    SL -.->|"correct one"| CS["Corrected slip<br/>own journals"]
    CS -.->|"carried"| BK
    OC["Off-cycle run"] --> BK
```

Components become a structure, a run creates the slips, the slips post one accrual, and the bank entry pays each salary in full. A correction or an off-cycle run takes the same path and pays only the difference.

```mermaid
flowchart LR
    RQ["Requisition<br/>approved"] --> JO["Job opening"]
    JO --> AP["Applicants through stages"]
    AP --> IV["Interviews, scorecards"]
    IV --> OF["Offer<br/>approved"]
    OF -->|"accepted"| EM["Employee"]
    EM --> OB["Onboarding checklist"]
    EM --> LV["Attendance, leave, payroll"]
    LV --> SP["Separation with notice"]
    SP --> FF["Full and final statement"]
    FF --> LEFT["Left, sign-in ends"]
```

An accepted offer makes the employee and starts onboarding. A separation records the notice, the employee works to the last day, and the full and final statement settles everything in one entry.

## 🧱 Data model

| Entity | What it is | Key fields or states |
|---|---|---|
| **Employee** | A person employed by a company | status (Active, Inactive, Suspended, Left), department, designation, reports to, shift, leave policy |
| **Department / Designation / Branch / Grade / Employment Type** | The organisation | parent, address, point and radius |
| **Employee Document** and **Type** | Contracts, IDs, certificates, with expiry | category, visibility, expiry, verified by |
| **Employee Letter** | The register of letters issued | type, serial, recipient |
| **Employee Onboarding / Separation / Promotion / Transfer** | Lifecycle documents | status, notice, last working day |
| **Checklist Template** | Tasks of an onboarding or offboarding | purpose, match, tasks |
| **Full and Final Statement** | What is owed each way on leaving | lines, net payable, journal entry |
| **Holiday List / Shift Type / Shift Assignment** | Working time | holidays, grace periods, thresholds |
| **Employee Check-in / Attendance / Attendance Request** | Time in and out | log type, place, status, hours |
| **Leave Type / Policy / Allocation / Application / Ledger Entry / Encashment** | Leave | accrual, carry forward, balance as a sum |
| **Job Requisition / Opening / Stage / Applicant / Interview / Offer** | Recruitment | stage category, score, offer status |
| **Appraisal Cycle / Template / Appraisal** | Performance | goals, weightage, scores |
| **Training Program / Event / Record** | Training | validity, result |
| **Expense Claim Type / Claim / Employee Advance** | Money employees spend | status, sanctioned amount |
| **Salary Component / Structure / Assignment** | Pay rules | type, formula, frequency |
| **Payroll Period / Income Tax Slab / Exemption Category / Declaration / Opening** | Tax | slabs, rebate, charges |
| **Payroll Lookup Table** | Rates and bands a formula reads | effective-dated rows |
| **Additional Salary / Leave Encashment** | One-off and repeating pay | amount, repeats, off-cycle |
| **Payroll Entry / Salary Slip** | A run and its slips | status, paid amount, held, replaces |
| **Employee Loan** | Money lent, repaid through salary | schedule, events, balance |
| **Recruitment Settings** | Offer approval bands | band limits and approvers |

Reports: 12 (see above). Print formats: the salary slip as a payslip. Scheduled jobs: attendance from check-ins (every minute), leave expiry, leave accrual, closing job openings, leavers, document expiry warnings and overdue checklist reminders.

## 🔌 Integrations

| With | How | Needs |
|---|---|---|
| **ERP** (connector `hr-erp`, packages `hr_erp` and `payroll_erp`) | Expense claims, advances, payroll accrual and bank entries, final settlements and loans post to the ledger; the company's letterhead is used on letters | ERP `>=0.1` |
| **Finance** | The company's bank account for the bank file | Finance installed, or give the account with the request |
| **Banks** | ISO 20022 `pain.001.001.09` and CSV files | The company's bank account |
| **Biometric and RFID devices** | They post check-ins by device id with an API key | A device or gateway that can call the endpoint |
| **Ferrum notifications, mail and documents** | Approvals, payslips and letters by email; document templates | Mail set up for the workspace |
| **Ferrum deprovisioning** | A leaver's sign-in ends on the relieving date, their work moves to their manager | None |
| **Studio** | Saved reports over slips drive payroll files | Studio and the example packs |
| **MCP and REST** | AI agents and scripts under each person's permissions | An API key or sign-in |
| **Country packs** | Statutory rules (contributions, taxes, returns) as formulas, tables and templates | Not shipped in this app |

## 📊 How it compares

People is compared with Workday HCM, SAP SuccessFactors, BambooHR, Rippling and Zoho People, and with the open-source Frappe HR. The big suites cover far more than People does; People's difference is payroll, expenses and the ledger in one system.

| Capability | Ferrum People | Workday | SuccessFactors | BambooHR | Rippling | Zoho People | Frappe HR |
|---|:-:|:-:|:-:|:-:|:-:|:-:|:-:|
| Employee records, org chart, directory | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Self-service portal | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Manager approvals inbox | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Leave policies, accrual and carry forward | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Leave as an append-only ledger | ✅ | 🟡¹ | 🟡¹ | 🟡¹ | 🟡¹ | 🟡¹ | ✅ |
| Team calendar of who is away | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 🟡¹ |
| Shifts and attendance from check-ins | ✅ | ✅ | ✅ | 🟡¹ | ✅ | ✅ | ✅ |
| Check-in with a place rule (GPS) | ✅ | 🟡¹ | 🟡¹ | 🟡¹ | ✅ | ✅ | ✅ |
| Biometric device vendor protocols | ❌ | 🟡¹ | 🟡¹ | 🟡¹ | 🟡¹ | ✅ | 🟡¹ |
| Native mobile apps | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Rostering grid and shift swaps | ❌ | 🟡¹ | 🟡¹ | 🟡¹ | 🟡¹ | ✅ | 🟡¹ |
| Overtime rules and comp-off | ❌ | ✅ | ✅ | 🟡¹ | ✅ | ✅ | 🟡¹ |
| Recruiting with stages and scorecards | ✅ | ✅ | ✅ | ✅ | ✅ | 🟡² | 🟡¹ |
| Requisition and offer approval | ✅ | ✅ | ✅ | 🟡¹ | ✅ | 🟡² | 🟡¹ |
| Job board multiposting, resume parsing | ❌ | ✅ | ✅ | ✅ | ✅ | 🟡² | ❌ |
| Interview scheduling with invites | ❌ | ✅ | ✅ | ✅ | ✅ | 🟡² | 🟡¹ |
| Onboarding and offboarding from templates | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Pre-boarding portal and e-signature | ❌ | ✅ | ✅ | ✅ | ✅ | 🟡¹ | 🟡¹ |
| Letters and certificates from templates | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 🟡¹ |
| Employee documents with expiry warnings | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | 🟡¹ |
| Payroll in the same product | ✅ | ✅ | ✅ | 🟡³ | ✅ | 🟡⁴ | ✅ |
| Payroll rules written as formulas and tables | ✅ | 🟡¹ | 🟡¹ | ❌ | 🟡¹ | 🟡¹ | ✅ |
| Statutory payroll for many countries | 🟡⁵ | ✅ | ✅ | ❌ | 🟡¹ | 🟡¹ | 🟡¹ |
| Off-cycle runs, arrears and one-slip corrections | ✅ | ✅ | ✅ | 🟡¹ | ✅ | 🟡¹ | 🟡¹ |
| Employee loans repaid through salary | ✅ | 🟡¹ | 🟡¹ | ❌ | 🟡¹ | ✅ | ✅ |
| Full and final settlement in one statement | ✅ | 🟡¹ | 🟡¹ | ❌ | 🟡¹ | ✅ | ✅ |
| Expense claims and advances | ✅ | ✅ | 🟡¹ | 🟡¹ | ✅ | 🟡⁴ | ✅ |
| Payroll and expenses post to the company ledger | ✅ | 🟡¹ | 🟡¹ | ❌ | 🟡¹ | 🟡¹ | ✅ |
| Bank file in ISO 20022 | ✅ | ✅ | 🟡¹ | ❌ | 🟡¹ | 🟡¹ | 🟡¹ |
| Appraisal cycles with goals and weights | 🟡⁶ | ✅ | ✅ | ✅ | 🟡¹ | ✅ | ✅ |
| 360 feedback, calibration, one-to-ones | ❌ | ✅ | ✅ | ✅ | 🟡¹ | ✅ | 🟡¹ |
| Compensation planning | ❌ | ✅ | ✅ | 🟡¹ | 🟡¹ | 🟡¹ | ❌ |
| Engagement surveys | ❌ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ |
| Benefits administration | ❌ | ✅ | ✅ | ✅ | ✅ | 🟡¹ | 🟡¹ |
| Learning and training records | 🟡⁷ | ✅ | ✅ | 🟡¹ | ✅ | ✅ | ✅ |
| Effective-dated history and "as of" reports | 🟡⁸ | ✅ | ✅ | 🟡¹ | ✅ | 🟡¹ | 🟡¹ |
| Reports and analytics dashboards | 🟡⁹ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Arabic and Hindi, right to left | ✅ | ✅ | ✅ | 🟡¹ | 🟡¹ | ✅ | ✅ |
| Open source | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ |
| Self-hosted | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ |
| One data set with accounting, projects and helpdesk | ✅ | 🟡¹ | 🟡¹ | ➖ | 🟡¹ | 🟡¹ | ✅ |
| No per-employee price | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | 🟡¹⁰ |

<sub>¹ Present in some form or not confirmed; check the product, country and edition. ² Through a separate product from the same vendor. ³ BambooHR's payroll is for the United States. ⁴ A separate vendor product that works with the HR product. ⁵ No country's rules are built into this app: they are formulas, lookup tables and templates, and a country pack is data. India's new regime is checked against published figures; statutory returns and contribution files are not built. ⁶ A cycle, a template of goals with weights, and self and manager scores; no review stages. ⁷ Programs, events and records with validity; no skills and no LMS link here (Ferrum has a separate learning app). ⁸ Promotions and transfers keep the previous values on the document; there is no "as of" date for the whole record. ⁹ Twelve reports; no dashboards. ¹⁰ Self-hosting is free; the vendor's cloud is priced per user.</sub>

### 🏆 Where People stands out

- **Payroll and the ledger are one system.** A run posts one balanced accrual, the bank entry pays each salary in full, and expenses, advances, loans and final settlements post alongside.
- **Payroll rules are data.** CEL formulas over employee, location, period, year-to-date and lookup tables mean a country's rules are written, not coded.
- **Put things right without breaking the books**: one-slip corrections, arrears, off-cycle runs, hold and release, carrying late changes, with nothing paid twice.
- **A full and final statement** that gathers everything and settles it in one entry.
- **Approvals as package defaults** for requisitions and offers, by salary band.
- **Letters, payslips and payroll files are templates** a workspace edits, with numbering series and a register.
- **Open source, self-hosted, no per-employee price**, with REST and MCP for agents.

### 🚧 Where the leaders are still ahead

- **Statutory depth for many countries**: contributions, returns, year-end forms and registers.
- **Talent suites**: 360 feedback, calibration, one-to-ones, compensation planning, surveys, benefits.
- **Recruiting reach**: job board multiposting, resume parsing, interview scheduling.
- **Mobile apps, geofenced clock-in apps, vendor biometric integrations and rostering.**
- **Effective-dated history** of every field, positions and job architecture.
- **Pre-boarding and e-signature** of contracts and policies.

> [!NOTE]
> Leader columns are from public product information as of November 2026 and may change. A 🟡 for a leader means the feature exists in some form or we could not confirm the details.

## 📍 Roadmap

### ✅ Recently shipped

- [x] **0.14.0** Job requisitions and offers ask for approval before they are submitted, as package defaults: the hiring manager's manager (else an HR Manager), and an HR Manager plus, by salary band, the people Recruitment Settings names.
- [x] **0.13.0** Letters, certificates and the payslip as templates a workspace edits, each issued by a button with its own numbering series, filed among the employee's documents and mailed; payroll files from a report over slips; names in Arabic and Hindi.
- [x] **0.12.0** Employee documents with expiry warnings, onboarding and offboarding from templates, recruiting stages, requisitions and scorecards, payroll formulas over lookup tables and year-to-date figures, check-ins with a place.
- [x] **0.11.0** Additional salary that repeats; late leave and attendance carried to the next pay; employee loans repaid through salary.
- [x] **0.10.0** One slip of a posted or paid run corrected; arrears for a backdated revision; off-cycle runs; salaries held and released.
- [x] **0.9.0** Employer contributions; opening year-to-date figures and a previous employer's; notice served before leaving; a full and final statement that gathers the last pay, leave, notice and gratuity.

### 🔜 Next up

**Pay**
- [ ] **Country packs for statutory payroll**: contributions, professional taxes, returns and year-end forms as data on top of the engine, one country at a time.
- [ ] **Pre-run checks and a month-on-month variance report**, with an approval step on the run.
- [ ] **Bulk revisions and increments** from a cycle or a sheet, with letters and arrears.
- [ ] **Monthly variable inputs** by import: incentives, overtime and deductions.
- [ ] **Reimbursements through the slip**, with a tax-free limit on proof.
- [ ] **Bank file layouts** for more banks.

**Time and leave**
- [ ] **Overtime rules**: multipliers by day type, approval, pay or compensatory leave, caps.
- [ ] **Compensatory leave**: a request for work on a holiday, approval and a credit with expiry.
- [ ] **Late-mark policies**: N late entries make a half day.
- [ ] **Rostering**: a grid, rotating patterns, shift swaps and open shifts.
- [ ] **Leave policy by rule** (grade, tenure, location), multi-step approval, delegation and escalation.
- [ ] **Optional holidays** the employee chooses, and holidays by branch.
- [ ] **A native or installable app** with push notices.

**People**
- [ ] **Employee change requests**: an employee edits their address, bank or emergency contact and HR approves.
- [ ] **Probation, confirmation and contract end** with reminders; rehire with the same sign-in.
- [ ] **Family, dependents, education and work history.**
- [ ] **Pre-boarding** forms and documents before day one, and **e-signature** of letters and contracts.

### 🔭 Later

- [ ] **Effective-dated history** of job, manager, pay and location, with "as of" reports and future-dated changes.
- [ ] **Positions and job architecture**: job families, levels, seats and vacancies on the org chart.
- [ ] **Interview scheduling** with invites; **job board multiposting** and a candidate status page; **talent pool** and duplicate merge; **recruiting analytics** (time to hire, source quality).
- [ ] **Goals and reviews**: review stages and deadlines, 360 feedback, calibration, one-to-ones and competencies; link to the Objectives app.
- [ ] **Compensation planning**: bands, compa-ratio and merit budgets.
- [ ] **Benefits** with enrolment and dependents; **flexible benefits**.
- [ ] **Surveys and eNPS**; **an HR helpdesk**; **assets issued to employees** with return at exit.
- [ ] **Expense policy** limits by type and grade, **travel requests**, receipt capture.
- [ ] **A CTC calculator** and past payslip import from the old system.
- [ ] **Offboarding clearance by department** with sign-off.
- [ ] **Dashboards**: attrition, tenure, absenteeism, cost of headcount; a **compliance calendar**.
- [ ] **Chat approvals** (Slack, Teams) and **direct bank payment** through a bank's API.
- [ ] **Multi-entity payroll** from one view; multi-currency pay.

## 🚀 Install

People is a native Ferrum app for platform `>=0.3.2, <0.4`.

1. In Ferrum, open the **App Manager** (or the store) and install **HR and Payroll**. The connector **HR and ERP** (expense claims, advances, payroll accounts, loans and final settlements) is offered where ERP is installed too, and ships with it. Self-hosters who build their own server image include the `hr` app with the others.
2. Give roles: **HR Manager** and **HR User** for HR, **Payroll Manager** for pay, **Employee** for everyone with a sign-in (link each employee to their user), **Leave Approver** and **Expense Approver** for managers, **Attendance Device** for API keys of devices.
3. Set up the organisation: departments, designations, branches (with a point and radius if you use place rules), grades, employment types and a **Holiday List**. Add **Shift Types** and **Leave Types** and a **Leave Policy**.
4. Add employees, or create them from sign-ins. Set their reporting line, leave and expense approvers, shift and policy.
5. For payroll with ERP: set the company's HR accounts (employee payable and advance, and loans and interest; see the company's Create HR and Payroll Accounts action), add **Salary Components** with their accounts, a **Salary Structure**, an **Income Tax Slab** and **Payroll Period** if you deduct tax, then **Salary Structure Assignments**. Run a **Payroll Entry**.
6. Edit the letter, certificate and payslip wording under Settings, Documents. Install the example packs if you want payroll files.
7. Publish job openings to put them on the careers page at `/people/careers/{workspace}`.

## 🔧 Develop

```bash
# an ordinary (non-superuser) role, so row-level security applies
createuser ferrum --pwprompt && createdb -O ferrum ferrum_hr_test

export FERRUM_HR_TEST_DATABASE_URL=postgres://ferrum:ferrum@localhost:5432/ferrum_hr_test
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The crate builds on [ferrum-app-erp](https://github.com/CBFerrum/ferrum-app-erp), which Cargo takes from GitHub. To work on both together, clone it next to this repository and patch it in `.cargo/config.toml` (git-ignored):

```toml
[patch."https://github.com/CBFerrum/ferrum-app-erp"]
ferrum-app-erp = { path = "../ferrum-app-erp" }
```

The interface lives in `ui/` and builds with Ferrum-Frontend's packages (clone Ferrum-Frontend into `ferrum/web` next to this repository, as CI does):

```bash
cd ui
npm run build       # ui/dist, checked against the size budget
npm run check
npm run typecheck
npm test
```

With a development server started with `FERRUM_DEV_UI=1`, `ferrum-app dev --ui` serves the pages from Vite behind the real bar and session, with hot reload.

`tests/hr_it.rs` and `tests/documents_it.rs` run the whole lifecycle against a real database: attendance and leave, recruitment, onboarding, payroll runs and corrections, loans, bank files, letters and documents. The interface tests draw each of People's screens on the server's side and check their words and dates.

## 📂 Project layout

```text
app.yaml                      the manifest: id, version, requirements, what it adds to Ferrum
connectors/erp/app.yaml       the connector with ERP (packages hr_erp and payroll_erp)
packages/hr/                  employees, attendance, leave, recruitment, appraisals, documents, letters
packages/hr_erp/              expense claims, advances, the full and final statement
packages/payroll/             salary components, structures, slips, runs, tax; print/ the payslip
packages/payroll_erp/         accounts, loans and the posting of runs
packs/                        two example packs for payroll files
src/                          attendance, leave, people, talent, documents, expense, settlement, reports, endpoints; payroll/ (slips, runs, formulas, bank file, loans)
ui/src/                       My space, approvals, calendar, directory, careers page; words in Arabic and Hindi
tests/                        integration tests against a real PostgreSQL
```

## 📜 Changelog

Recent releases are listed above; every version is in [CHANGELOG.md](CHANGELOG.md).

## 🤝 Contributing

Issues and pull requests are welcome at [github.com/CBFerrum/ferrum-app-hr](https://github.com/CBFerrum/ferrum-app-hr/issues). Please run the checks in Develop first. Keep country rules out of the code: they belong in formulas, tables and templates.

## 📄 License

AGPL-3.0-only. See [LICENSE](LICENSE).

<div align="center">

<sub>Part of <a href="https://github.com/CBFerrum/Ferrum">Ferrum</a>, the open business platform · Made with 🦀 Rust and 🐘 PostgreSQL</sub>

</div>
