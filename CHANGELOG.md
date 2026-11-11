# Changelog

## 0.11.2

The interface follows the upgraded design system, rebuilt with Ferrum-Frontend 0a47157. Text uses the kit's type roles (nothing under 12px) and controls its height tokens; loading shows skeletons shaped like what is coming; empty, no matches, denied, offline and failed are told apart; status colours come from stored state ids. The interface check now runs the UI lint against an empty baseline. A request's colour comes from its stored state, not from the wording shown.

## 0.11.1

A salary slip prints as a payslip: the company, the period, the employee, the days, earnings and deductions side by side with arrears, adjustments and loan instalments, employer contributions apart, net pay.
Draft, Cancelled, Off-cycle, Final settlement and held salaries are marked on it. Payroll 0.5.1; nothing else changes.

## 0.11.0

For a company moving its payroll here with its loans and its late leave.

- **Additional salary that repeats.** Repeats, with Until (empty: until stopped) and Every
  (Periods): paid once in every regular slip in range, or every N-th. Stop or Change stops it from
  the next period not yet paid, or carries it on there at a new amount (a new additional salary
  keeping its cycle). A correction of a paid period still pays it at that period's amount; its tax
  tick applies to each payment. Refused: one that repeats off-cycle, cancelling one a slip paid.
- **Leave and attendance changed after the payroll ran.** A change by a person that alters what a
  submitted regular slip paid (leave without pay approved or cancelled, an absence marked or removed,
  a half day, an attendance request) is refused, naming the slip and the days, until someone who
  submits slips chooses on that slip (Changes After Payroll): Carry to Next Pay, or Correct the
  Slip. A change that alters nothing paid goes through. Carried, the next slip (or an off-cycle one)
  has a Pay Adjustment: the period, the days, its day's pay as paid, the amount and the reason, part
  of gross pay and posted to the salary expense account, carried once; a correction of the period
  pays its days as they are and takes back what was carried. Slips show what still waits (Changed
  Since Submitted, a filter); a leaver's last slip carries all of it.
- **Employee loans** (with the connector to ERP). Paid from the bank on their day (Employee Loans
  debited), repaid from the first month over a number of months or an amount a month, interest on
  the balance left at a yearly rate (none by default). Each monthly slip deducts the instalment due,
  principal and interest apart (the loan and Interest on Employee Loans credited), never more than
  the pay allows: the rest is due with the next. Defer an Instalment (with a reason), Repay Outside
  Salary (in part, or all of it: paid off), Write Off (to an account, with a reason). A slip
  corrected or cancelled gives its instalment back. The schedule shows what was deducted, deferred
  and is due; the balance is what the loan account holds. A leaver's balance is a recovery on the
  full and final statement and is closed by its entry. Refused: an amount a month below the first
  month's interest, cancelling a loan repaid in part, deferring a month already deducted.

What an upgrading workspace sees: each slip already submitted has no choice made, so a change to a
paid month is refused until one is made on its slip. The Employee Loans and Interest on Employee
Loans accounts are made on the company when the first loan needs them (or Create HR and Payroll
Accounts); set others on the company if the chart has its own. Additional salary already made is
one-off, as before. Names of the new models, fields and actions in Arabic and Hindi (translated by
the model, to be reviewed by native speakers).

Not yet: a list of what is waiting beyond the slip list's filter; an employee advance recovered
through salary; loans in My space; a loan topped up; loans repaid weekly or fortnightly (monthly
only); an instalment taken by an off-cycle slip.

## 0.10.0

For a company moving its payroll here with nothing to put right by hand.

- **One person's pay corrected after the run is posted.** A slip of a payroll entry whose slips are
  submitted has Correct: the slip is cancelled with its share of the run's accrual taken back (an
  entry of its own, today), and a draft for the same period is made in its place, for the person to
  put right (attendance, leave, additional salary of that period) and submit. Submitted, it posts its
  own accrual, by the employee's cost centre as the run's, with its employer contributions; the
  cancelled slip stays, Replaced By the correction, which Replaces it, and the run's row shows the
  correction. Nobody else's slip or posting is touched. When the run was already paid, nothing sent
  to the bank is undone: the correction takes over what the bank paid of the slip it replaces, so a
  higher one leaves the difference owed (paid with the next bank payment that lists the employee, or
  an off-cycle one) and a lower one leaves an excess, taken back from their next slip of a payroll
  run as Excess Pay Recovered, never more than that pay; what it cannot take is shown as Still to
  Recover and taken from later pay. The payroll payable account holds what the slips still owe at
  every step. Later slips' income tax counts the corrected slip, not the cancelled one. Refused:
  cancelling such a slip directly (Correct it, or cancel the payroll entry), correcting a slip
  already corrected.
- **Arrears.** When a salary assignment takes effect in periods already paid (a revision entered
  late), the employee's next slip works each such period out again as it was paid (its working and
  payment days, leave without pay, absences and proration, every component that depends on another),
  with the assignment in force now, and pays each component's difference as arrears; a lower one is
  taken back the same way. The breakdown is kept on the slip (Arrears: period, component, as paid,
  due now) and each component's total is in its row (Arrears). Arrears are worked out from what is
  submitted every time a slip is saved, so the same revision and period never pay twice, a slip made
  again carries them once, a period corrected after its arrears were paid counts what was paid, and
  a revision withdrawn before anything paid it leaves nothing behind. Refused: withdrawing an
  assignment a submitted slip's arrears were worked out from.
- **Income tax.** Arrears raise the year's projected tax as additional salary does, spread over the
  periods left as before. An additional salary can have its full tax deducted in the period it is
  paid instead (Deduct its full tax in the period it is paid, off by default); an off-cycle slip
  always deducts the full tax of what it pays, as no later period counts on it. A leaver's last
  slip settles the year as before. The year's tax is the same, to the cent, as when the same pay is
  given without a correction or a backdate.
- **Off-cycle pay.** A payroll entry or a salary slip can be Off-Cycle: for one day (its start date)
  and, for a run, the employees listed. Its slips pay only Additional Salary marked Off-Cycle with
  that payroll date, and arrears, with the tax they bring; never the period's regular pay, which an
  off-cycle slip does not count twice and does not stand in the way of. Get Employees lists those
  with an off-cycle additional salary on the day or something still owed from a corrected or
  released slip, or arrears due; others can be added by hand. They post, pay and appear in the bank file and salary register
  as any slip does, and later slips' tax counts them. Refused: a second off-cycle slip for the same
  employee and day; an off-cycle additional salary whose day an off-cycle slip already paid.
- **Held salaries.** A slip of a payroll run can be held (Hold Salary, with a reason) until it is
  paid: it is computed and accrued as everyone's, shown as held on the slip and the run's row, and
  left out of the bank entry, the bank file and the bank remittance report. Release Salary pays it
  with the next bank payment that lists the employee (the next run's, an off-cycle one, or its own
  run's again), and a full and final statement pays it (and anything else still owed or paid too much
  on their run slips) on a line of its own (Payroll Salary), held or not. A run is Paid once every
  slip of it is.
- **A run paid outside Ferrum** (by the company's own journal entry, or matched on the bank
  statement) has Record Payment Made Elsewhere (a date and a reference): each of its slips not yet
  paid and not held counts as paid by that reference, nothing is posted (the company's own entry
  is), and the run shows it and is Paid. A correction after it pays or recovers only the difference.
  Undo Payment Made Elsewhere takes it back while no correction counts what it paid. Refused: Make
  Bank Entry for what was recorded as paid elsewhere (it says so), and cancelling the run before the
  record is undone.
- **The bank entry and the bank file pay the same**: each salary of the run in full, with what is
  still owed on the employee's corrected or released slips of other runs, one transfer each. What a
  bank entry pays each slip is kept (Paid Amount on the slip). Refused: a payroll bank entry for
  another amount than what is due, and submitting one made before what is due changed (make it again).

Data already there: a payroll entry's bank entries made before are shared out over its slips in
the order the bank file lists them, once, on upgrade (payroll_erp/sql/001), so their slips are paid
and a run paid in part has its first slips paid. Slips' arrears tables start empty and their rows'
Arrears at nothing. Salary slips made by hand outside a payroll entry, and a statement's own slip,
are paid as before, outside these payments. Additional salary keeps its tax spread as before
(Deduct its full tax… starts off). Make Bank Entry stays on a run once it is paid and says there is
nothing left to pay. Names of the new model, fields, actions and option in Arabic and Hindi (46 each
in ui/src/locales/names, translated by the model, to be reviewed by native speakers).

An upgrading workspace that pays salaries outside Ferrum records each run already paid that way
(Record Payment Made Elsewhere) before correcting any of its slips: until then Ferrum counts them
unpaid, and a correction would pay the whole corrected slip.

Not yet: a revision that takes effect within a period counts for the whole of it, as a slip made now
for that period would (no split by days); arrears for a component additional salary replaced in a
period are not worked out.

## 0.9.0

For a company moving its HR and payroll here, and its leavers.

- **Employer contributions.** A salary component can be an Employer Contribution (the employer's
  share of provident fund, insurance or social security), in a salary structure's third table,
  Employer Contributions. It is computed like any other component, after the deductions (formula,
  condition, order, proration; a wage ceiling is written in the formula, as
  `min(BS * payment_days / working_days, 15000) * 0.12` for India's provident fund on the basic
  earned), and is never part of gross or net pay: the slip shows it in its own section and its
  Total Employer Contribution, and the salary register and payroll summary in their own columns
  after the deductions. A run posts it with the accrual: the cost debited (the component's expense
  account for the company, else the default salary expense account, by the employee's cost
  centre, as earnings are) and the liability it is owed on credited; cancelling the run reverses
  it. Refused: a contribution in the earnings or deductions (or an earning in the contributions),
  and a run whose contribution has no account for the company.
- **Opening Payroll Figures.** Per employee and payroll year: the taxable earnings and income tax of
  the time before the first slip made here (what the system you moved from paid), and a previous
  employer's taxable income and tax in that year. Income tax on every later slip counts them, so a
  company that moves here mid-year deducts what the year's tax calls for, and a joiner is taxed on
  the year's whole income with credit for the tax already paid. They can be imported like other
  records (Payroll Manager). Refused: figures reaching into months a slip submitted here already
  pays, a second set for the same year, a day outside the payroll year, and a slip starting on or
  before their last day (so nothing counts twice). The Income Tax Deductions report shows them on
  columns of their own (taxable income and tax before here, a previous employer's income and tax)
  and adds them into each employee's Year's Taxable Income and Year's Income Tax, which for a payroll
  year are what the last slip's income tax counted; an employee with opening figures and no slip in
  the period is listed too.
- **A leaver's tax.** The slip of the period an employee leaves in (their relieving date within it)
  settles the year: the tax on what the year actually paid them, less what was deducted, instead of
  spreading the year's projection over months they will not work. The slip says so (Final
  Settlement).
- **Notice.** A separation carries the day notice was given (Notice Given On, which was the
  resignation letter date), the notice due (the employee's new Notice Period, else the company's;
  none on a death, retirement or contract end) and the Last Working Day (which was the relieving
  date), and works out the notice served and short. Death is a reason. Submitted, the employee stays
  at work until the last working day: still Active, paid (the last month prorated to the day),
  accruing leave, on the calendars and in the directory, which shows those who see every employee
  that they are serving notice and until when. The upkeep makes them Left once the day has passed,
  and then ends their sign-in (it was ended on the day itself); a day already past makes them Left at
  once. Withdrawn (cancelled), the separation puts them back, unless a submitted full and final
  statement settles it. Notice short is recovered from the employee (the default when they resign),
  paid in lieu (the default when the employer ends it) or waived.
- **The full and final statement gathers it all.** Get Outstanding Amounts now adds, before the
  claims and advances it gathered: the pay earned up to the last working day that no slip covers yet
  (a salary slip of its own for that pay period, which settles the year's tax; it is submitted with
  the statement, posting its accrual, and paid by it from payroll payable); each encashable leave
  type's balance left that day (at the structure's encashment amount a day, else a day's pay, used
  up from the balance when the statement is submitted and given back when it is cancelled); notice
  recovery or notice pay at a day's pay (the last slip's full gross over its working days); and
  gratuity by the company's rule. Each gathered line keeps what was gathered (Gathered) beside the
  amount, which a person may change with a Reason; gathering again keeps their change. Refused: a
  changed amount without a reason, a last-pay line that is not its slip's net pay (change the slip
  with an additional salary and gather again), more leave than is left, and a pay period before the
  last working day that no slip pays.
- **Gratuity**, on the company (with the connector to ERP): Gratuity After (Years), Gratuity Days per
  Year, Gratuity Days in a Month, Gratuity Pay (a formula over the last slip's components, gross when
  empty) and Gratuity Part Years (more than six months counting as a year, completed years only, or
  pro rata). No country's rule is built in: India's 15 days' pay a year on a 26-day month after five
  years is 5, 15, 26, `BS` (with DA where paid) and the first rounding. A death waives the minimum.
- **Where things are.** The full and final statement is in HR beside Employee Separation and in
  Payroll (no longer under Expense Claims); Leave Encashment is under Attendance & Leave; Opening
  Payroll Figures under Payroll. Their routes and permissions are as they were. My space links the
  person's payslips to their tax exemption declarations.

Data already there: components keep their types and slips their rows (Employer Contributions starts
empty). Employees an earlier release made Left on submitting a separation whose last working day has
not passed are put back to Active by a backfill (`serving_notice`, at the hourly upkeep), and become
Left after it; a separation made before has no notice figures until it is saved again. A statement
made before has no gathered amounts, so its lines need no reason. Names of the new models and fields
in Arabic and Hindi (855 each in ui/src/locales/names, translated by the model, to be reviewed by
native speakers); rebuilt with Ferrum-Frontend e065883.

An upgrading workspace sets: the company's Notice Period (Days) (and an employee's own where it
differs), its gratuity rule (none is paid until Gratuity Days per Year is set), and for each
employer contribution component an account for each company.

Not yet: a leave allocation granted for the whole year is not prorated when someone leaves part-way;
tax on leave encashment and gratuity paid through the statement (they are not on a slip, so not
taxed; India exempts both within limits).

## 0.8.14

The reports it defines in code (their titles and parameters) have names in Arabic and Hindi too, with its models' and manifest's (797 each in ui/src/locales/names). Translated by the model, to be reviewed by native speakers.

## 0.8.13

Rebuilt with Ferrum-Frontend c59c9a2: the names of its models, fields and manifest in Arabic and Hindi (795 each in ui/src/locales/names, which the platform serves to every page of the workspace, so a record's labels, the launcher and the bar read in the person's language), and the sentences its pages still showed in English (split around a link or a figure, built in code from a template or word by word) whole and translated (263 words each in ui/src/locales). Translated by the model, to be reviewed by native speakers.

## 0.8.12

Rebuilt with Ferrum-Frontend 155518c: the sentences its pages built in code (counts, dates, what a toast or a screen reader says) go through t() too, as whole sentences where code chose a word, and carry built-in Arabic and Hindi (233 words each in ui/src/locales, translated by the model and to be reviewed by native speakers).

## 0.8.11

The assistant knows what its records mean in its pages (contributes.shell.assistant): their states, what counts and where figures come from, with questions it suggests asking there. Personal and health details are in field groups: Employee.date_of_birth (personal), Employee.blood_group (personal), EmployeePromotion.revised_ctc (pay), JobOffer.ctc (pay). Every role that read or wrote them is granted the group, so people see what they saw, and an AI provider gets them only where the workspace's egress policy allows the group.

Its reports keep to the workspace's egress policy too (FERRUM-AI-003): run for an AI provider the workspace has not allowed them, upcoming events leave birthdays out, and the income tax and bank reports, which read salary slips and bank details, are refused. Two tests expected the platform's old word for a duplicate and now read its message, which names the fields ("another Job Applicant already has this Email Id and Job Opening").

## 0.8.10

Rebuilt with Ferrum-Frontend 5b22e7b: its own words carry built-in Arabic and Hindi (207 words each, ui/src/locales, translated by the model and to be reviewed by native speakers), fetched for the page's language only. The labels it keeps in objects, its dialogs, its page titles and the words it keeps in variables go through t() as well, and the names and sizes it shows as they are keep their order right to left. Its pages are drawn once their words are in, and its entry carries only the parts of the kit it uses.

## 0.8.9

Rebuilt with Ferrum-Frontend 6537d65: an action's link, code or token shown once is kept in a dialog to copy, and on a page that reads right to left, English left untranslated keeps its own order. Its own words go through the kit's t() (169 places), so a workspace's own words for its language reach them, and Admin, Interface words lists them under it (dist/words.json).

## 0.8.8

Rebuilt with Ferrum-Frontend d5d98f2: a workspace's own words for its interface (Admin, Interface words) reach its pages as they load; an attachment field that holds a picture shows it; and a value another record already has is said so, where the form said the record had been changed by someone else. Its own screens' margins, paddings, borders, alignment and insets go by the line's start and end, so they mirror right to left as the kit's do; what it places by coordinates (boards, timelines, charts, handles) stays as it was.

## 0.8.7

Rebuilt with Ferrum-Frontend 63280d3: right to left, its pages mirror as they read (icons that point along the line, margins and alignment by the line's start and end, arrow keys along it), and the records pages' words are in Arabic and Hindi.

## 0.8.6

Rebuilt with Ferrum-Frontend a64416a: a new record's box whose default is 1 shows ticked, as it is saved.

## 0.8.5

Rebuilt with Ferrum-Frontend 922fcfb: an action's dialog says every refusal, the errors on what the action made (another record's rows) among them, and an action with parameters and a confirmation asks once.

## 0.8.4

Rebuilt with Ferrum-Frontend a76295e: its pages show times on a person's own clock when they chose one on their Account page, while today and due dates stay on the workspace's day.

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
