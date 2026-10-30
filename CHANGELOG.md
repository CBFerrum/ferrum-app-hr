# Changelog

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
