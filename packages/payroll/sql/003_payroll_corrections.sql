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
