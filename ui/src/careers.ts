// The careers page's application form: what to check before sending, in the server's own terms
// (which checks them again), so that a mistake is told next to its field and not after a wait.
// Pure, so it is tested without a browser.

export interface Application {
  name: string;
  email: string;
  phone: string;
  resume_link: string;
  cover_letter: string;
  /** A field no person sees: a program that fills it in is not a person. */
  website: string;
}

export const EMPTY: Application = { name: "", email: "", phone: "", resume_link: "", cover_letter: "", website: "" };

export const LIMITS = { name: 120, email: 254, phone: 40, resume_link: 500, cover_letter: 5000 } as const;

/** Whether this could be an address: something, one `@`, and a domain with a dot. */
export const looksLikeEmail = (v: string): boolean => /^[^\s@]+@[^\s@.]+(\.[^\s@.]+)+$/.test(v.trim());

/** Whether this is a web address a person can follow. */
export const looksLikeLink = (v: string): boolean => /^https?:\/\/[^\s]+$/i.test(v.trim()) && v.trim().length <= LIMITS.resume_link;

/** What is wrong with the form, field by field. Nothing is wrong when it is empty. */
export function problems(a: Application): Partial<Record<keyof Application, string>> {
  const out: Partial<Record<keyof Application, string>> = {};
  if (!a.name.trim()) out.name = "Tell us your name";
  else if (a.name.trim().length > LIMITS.name) out.name = `Use at most ${LIMITS.name} characters`;
  if (!a.email.trim()) out.email = "Tell us your email address, so that we can reply";
  else if (!looksLikeEmail(a.email) || a.email.trim().length > LIMITS.email) out.email = "This does not look like an email address";
  if (a.phone.trim().length > LIMITS.phone) out.phone = `Use at most ${LIMITS.phone} characters`;
  if (a.resume_link.trim() && !looksLikeLink(a.resume_link)) out.resume_link = "Give a web address that starts with https://";
  if (a.cover_letter.trim().length > LIMITS.cover_letter) out.cover_letter = `Use at most ${LIMITS.cover_letter} characters`;
  return out;
}

/** What goes to the server: trimmed, and without what was left empty. */
export function payload(a: Application): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(a)) if (v.trim()) out[k] = v.trim();
  return out;
}

/** "Support · Chennai · Full-time", for a card. */
export const meta = (o: { department?: string | null; branch?: string | null; employment_type?: string | null }): string =>
  [o.department, o.branch, o.employment_type].filter(Boolean).join(" · ");
