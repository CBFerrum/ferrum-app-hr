-- Business keys that must ignore cancelled documents (YAML unique keys and exclusions cover every row,
-- which would block amending a cancelled document). The hooks check the same rules with friendlier
-- messages; these constraints close the race between two concurrent saves.

create unique index uq_attendance_employee_day on e_attendance (tenant_id, employee, attendance_date) where docstatus < 2;

alter table e_shift_assignment add constraint ex_shift_assignment_submitted
  exclude using gist (tenant_id with =, employee with =, daterange(start_date, end_date, '[]') with &&) where (docstatus = 1);

alter table e_leave_allocation add constraint ex_leave_allocation_submitted
  exclude using gist (tenant_id with =, employee with =, leave_type with =, daterange(from_date, to_date, '[]') with &&) where (docstatus = 1);

create unique index uq_appraisal_employee_cycle on e_appraisal (tenant_id, employee, appraisal_cycle) where docstatus < 2;
