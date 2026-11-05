-- One full and final statement per separation, cancelled ones aside (amending makes a new one).
-- The hook says so first; this closes the race between two saves.
create unique index uq_full_and_final_separation on e_full_and_final_statement (tenant_id, separation) where docstatus < 2;
