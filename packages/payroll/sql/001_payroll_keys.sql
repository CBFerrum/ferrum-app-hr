-- Business keys that must ignore cancelled documents (YAML unique keys and exclusions cover every row,
-- which would block amending a cancelled document). The hooks check the same rules with friendlier
-- messages; these constraints close the race between two concurrent saves.

alter table e_salary_slip add constraint ex_salary_slip_employee_period
  exclude using gist (tenant_id with =, employee with =, daterange(start_date, end_date, '[]') with &&) where (docstatus < 2);

create unique index uq_salary_structure_assignment_day on e_salary_structure_assignment (tenant_id, employee, from_date) where docstatus = 1;

create unique index uq_salary_structure_name on e_salary_structure (tenant_id, company, structure_name) where docstatus < 2;

create unique index uq_tax_declaration_period on e_employee_tax_exemption_declaration (tenant_id, employee, payroll_period) where docstatus < 2;
