# Ferrum HR and Payroll

A native app for [Ferrum](https://github.com/CBFerrum/Ferrum), the open-source, AI-first ERP. Two
packages: `hr` and `payroll`.

- Organization (departments, designations, branches, grades), employees with reporting lines and their own self-service access
- Holiday lists, shifts and assignments, check-ins from devices, attendance built from check-ins (late entry, early exit, half days, overnight shifts)
- Leave types and policies, allocations, applications with approval workflow and a leave ledger, encashment and carry forward
- Expense claims and employee advances with approvals, balanced journal postings and settlement
- Recruitment (openings, applicants, interviews, offers → employees), appraisals with weighted goals, onboarding, separation, promotions and transfers
- Payroll: salary components with formulas (CEL), structures and assignments, tax slabs and exemption declarations, payroll entries that create slips, post one balanced accrual and pay by bank entry
- Eleven reports, from the monthly attendance sheet to the salary register and bank remittance

## Install

Workspace administrators install it from **Apps** in Ferrum; it ships in the official Ferrum
server image. Self-hosters building their own image enable the `hr` feature of the server.
Everything it adds is also available to AI agents over Ferrum's MCP server.

## Develop

Clone it next to the platform and point the platform dependencies at your checkout (this file
is git-ignored):

```toml
# .cargo/config.toml
[patch."https://github.com/CBFerrum/Ferrum"]
ferrum-kernel = { path = "../ferrum/crates/kernel" }
ferrum-meta = { path = "../ferrum/crates/meta" }
ferrum-domain = { path = "../ferrum/crates/domain" }
ferrum-calc = { path = "../ferrum/crates/calc" }
```

The tests need PostgreSQL with an ordinary (non-superuser) role — row-level security must apply:

```bash
createuser ferrum --pwprompt && createdb -O ferrum ferrum_hr_test
```

```bash
cargo test
```

`FERRUM_HR_TEST_DATABASE_URL` overrides the database URL. The app's metadata lives in `packages/` and is embedded
at build time; `app.yaml` is its manifest (see Ferrum's `docs/plan/apps.md`).

## License

AGPL-3.0-only, like the platform.
