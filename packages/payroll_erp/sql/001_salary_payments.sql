-- Bank entries made before HR and Payroll 0.10.0 paid their payroll entry as a whole. Each slip's share
-- of them, in the order the bank file lists the slips (an entry paying part of a run paid the first
-- slips first), so that a slip corrected or an entry cancelled later knows what was paid. Entries that
-- already have their shares are left alone, and so are slips whose paid amount is already known: it
-- can run again. Every workspace's rows.
select set_config('ferrum.system', 'on', true);

insert into pay_salary_payment (tenant_id, source, salary_slip, amount)
select s.tenant_id, j.id, s.id, least(s.hi, j.hi) - greatest(s.lo, j.lo)
from (select tenant_id, id, payroll_entry, sum(net_pay) over w - net_pay as lo, sum(net_pay) over w as hi
      from e_salary_slip where docstatus = 1 and payroll_entry is not null and net_pay > 0
      window w as (partition by tenant_id, payroll_entry order by employee_name, number, id)) s
join (select tenant_id, id, run, sum(paid) over w - paid as lo, sum(paid) over w as hi
      from (select je.tenant_id, je.id, je.posting_date, je.created_at, pe.id as run,
                   (select coalesce(sum(a.debit - a.credit), 0) from e_journal_entry__accounts a
                     where a.tenant_id = je.tenant_id and a.parent_id = je.id and a.account = pe.payroll_payable_account) as paid
            from e_journal_entry je
            join e_payroll_entry pe on pe.tenant_id = je.tenant_id and pe.id::text = je.ext->>'hr_reference'
            where je.docstatus = 1 and je.ext->>'hr_reference_type' = 'Payroll Entry'
              and not exists (select 1 from pay_salary_payment p where p.tenant_id = je.tenant_id and p.source = je.id)) x
      where paid > 0
      window w as (partition by tenant_id, run order by posting_date, created_at, id)) j
  on j.tenant_id = s.tenant_id and j.run = s.payroll_entry
where least(s.hi, j.hi) > greatest(s.lo, j.lo);

update e_salary_slip s set paid_amount = p.paid
from (select tenant_id, salary_slip, sum(amount) as paid from pay_salary_payment group by tenant_id, salary_slip) p
where p.tenant_id = s.tenant_id and p.salary_slip = s.id and s.paid_amount is null;
