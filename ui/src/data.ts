// Reading records the way People's screens do: a list of one entity, filtered, with the titles of
// what its fields link to; and whether the person may read an entity at all (an app that is not
// installed, or a right they were not given, hides its section instead of failing).

import { api } from "@ferrum/client";
import { useMetaIndex } from "@ferrum/records";

/** A record that carries the names of what its link fields point at. */
export interface Titled {
  _titles?: Record<string, string>;
}

export const rows = <T,>(entity: string, query: Record<string, string | number>, signal?: AbortSignal) =>
  api.get<{ rows: (T & Titled)[]; total?: number }>(`/api/v1/${entity}`, { titles: "true", ...query }, signal);

/** Filters as the list endpoint takes them: `[["employee", "=", id]]`. */
export const f = (filters: unknown[]): string => JSON.stringify(filters);

/** What a link field points at, by name. */
export const title = (r: Titled, field: string): string | undefined => r._titles?.[field];

/** Whether the person may read the entity: true or false, and undefined until it is known. */
export function useCanRead(entity: string): boolean | undefined {
  const meta = useMetaIndex().data;
  if (!meta) return undefined;
  const e = meta.entities.find((x) => x.name === entity);
  return !!e && e.permissions.read !== "none";
}
