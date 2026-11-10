-- One set of opening figures per employee and payroll year (amending a cancelled one is allowed).
create unique index uq_payroll_opening_period on e_payroll_opening (tenant_id, employee, payroll_period) where docstatus < 2;
