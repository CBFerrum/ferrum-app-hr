// People's own screens, at the top of each: your space, the requests waiting for you, the team
// calendar and the people. The count of requests waiting is the same list the approvals page
// shows, so a decision anywhere updates it at once.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback } from "react";
import { NavLink } from "react-router";
import { api } from "@ferrum/client";
import { useLiveEntity } from "@ferrum/kit";
import { useMe } from "@ferrum/records";
import type { Item } from "./inbox";

export const INBOX = ["people", "inbox"];

/** The requests routed to the signed-in person, oldest first. */
export function useInbox() {
  const qc = useQueryClient();
  const refresh = useCallback(() => void qc.invalidateQueries({ queryKey: INBOX }), [qc]);
  useLiveEntity("LeaveApplication", refresh);
  useLiveEntity("AttendanceRequest", refresh);
  useLiveEntity("ExpenseClaim", refresh);
  return useQuery({ queryKey: INBOX, queryFn: ({ signal }) => api.get<{ items: Item[] }>("/api/app/hr/inbox", undefined, signal).then((r) => r.items) });
}

/** Roles whose people decide on others' requests. */
const DECIDERS = ["Leave Approver", "Expense Approver", "HR Manager"];

export function Nav() {
  const me = useMe().data;
  const waiting = useInbox().data?.length ?? 0;
  const decides = !!me && (me.user.is_admin || me.user.roles.some((r) => DECIDERS.includes(r)));
  return (
    <nav className="ppl-tabs" aria-label="People">
      <NavLink to="/" end>
        My space
      </NavLink>
      {(decides || waiting > 0) && (
        <NavLink to="/approvals">
          Approvals
          {waiting > 0 && (
            <span className="ppl-count" aria-label={`${waiting} waiting`}>
              {waiting}
            </span>
          )}
        </NavLink>
      )}
      <NavLink to="/calendar">Calendar</NavLink>
      <NavLink to="/directory">People</NavLink>
    </nav>
  );
}
