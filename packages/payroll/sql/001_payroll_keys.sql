-- Business keys that must ignore cancelled documents (YAML unique keys and exclusions cover every row,
-- which would block amending a cancelled document). The hooks check the same rules with friendlier
-- messages; these constraints close the race between two concurrent saves.

-- One regular salary slip per employee and period, and beside it off-cycle slips: one per employee and day.
alter table e_salary_slip add constraint ex_salary_slip_employee_period
  exclude using gist (tenant_id with =, employee with =, (coalesce(off_cycle, false)) with =, daterange(start_date, end_date, '[]') with &&)
  where (docstatus < 2);

create unique index uq_salary_structure_assignment_day on e_salary_structure_assignment (tenant_id, employee, from_date) where docstatus = 1;

create unique index uq_salary_structure_name on e_salary_structure (tenant_id, company, structure_name) where docstatus < 2;

create unique index uq_tax_declaration_period on e_employee_tax_exemption_declaration (tenant_id, employee, payroll_period) where docstatus < 2;

-- One set of opening figures per employee and payroll year (amending a cancelled one is allowed).
create unique index uq_payroll_opening_period on e_payroll_opening (tenant_id, employee, payroll_period) where docstatus < 2;

-- What has been paid of each salary slip of a payroll run, and by what (`source`): a bank entry, a full
-- and final statement's entry, or a later slip that took back what was paid too much (negative). A
-- correction takes over the slip it replaces' rows.
create table pay_salary_payment (
  tenant_id uuid not null references tenant(id) on delete cascade,
  source uuid not null,
  salary_slip uuid not null,
  amount numeric not null,
  primary key (tenant_id, source, salary_slip)
);
create index pay_salary_payment_slip on pay_salary_payment (tenant_id, salary_slip);
alter table pay_salary_payment enable row level security;
alter table pay_salary_payment force row level security;
create policy tenant_isolation on pay_salary_payment using (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()))
  with check (tenant_id = (select ferrum_tenant()) or (select ferrum_is_system()));
