// The careers page, open to anyone without signing in (/people/careers/{workspace}): the openings
// HR has published, one opening in full, and an application form. Everything comes from the
// workspace's public endpoints; the server checks every field again and keeps one application from
// the same address for each opening.

import { useMutation, useQuery } from "@tanstack/react-query";
import { type FormEvent, useState } from "react";
import { Link, useParams } from "react-router";
import { api, ApiError, bootstrap, errorMessage, formatDate } from "@ferrum/client";
import { Alert, Button, EmptyState, ErrorState, Field, Markdown, Spinner, useShellTitle } from "@ferrum/kit";
import { type Application, EMPTY, LIMITS, meta, payload, problems } from "./careers";

interface Opening {
  id: string;
  title: string;
  designation: string | null;
  department: string | null;
  branch: string | null;
  employment_type: string | null;
  company: string | null;
  posted_on: string | null;
  closes_on: string | null;
  description: string | null;
}

const base = (slug: string) => `/api/site/${encodeURIComponent(slug)}/hr/careers`;

function Form({ slug, opening, onSent }: { slug: string; opening: Opening; onSent: (name: string) => void }) {
  const [a, setA] = useState<Application>(EMPTY);
  const [seen, setSeen] = useState(false);
  const [told, setTold] = useState<Record<string, string>>({});
  const mine = problems(a);
  const send = useMutation({
    mutationFn: () => api.post(`${base(slug)}/${opening.id}/apply`, payload(a), { idempotencyKey: crypto.randomUUID() }),
    onSuccess: () => onSent(a.name.trim()),
    onError: (e) => {
      if (e instanceof ApiError && e.fields.length) setTold(Object.fromEntries(e.fields.map((f) => [f.field, f.message])));
    },
  });
  const field = (k: keyof Application) => (seen ? (mine[k] ?? told[k]) : told[k]);
  const set = (k: keyof Application) => (e: { target: { value: string } }) => {
    setA((x) => ({ ...x, [k]: e.target.value }));
    setTold((t) => ({ ...t, [k]: "" }));
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    setSeen(true);
    if (Object.keys(mine).length) return (document.querySelector("[aria-invalid=true]") as HTMLElement | null)?.focus();
    send.mutate();
  };
  return (
    <form className="stack ppl-apply" onSubmit={submit} noValidate aria-labelledby="apply-title">
      <h2 id="apply-title" className="ppl-sub">
        Apply for this role
      </h2>
      {send.error && !(send.error instanceof ApiError && send.error.fields.length && send.error.fields.every((f) => f.field in EMPTY)) && <Alert tone="error">{errorMessage(send.error)}</Alert>}
      <Field label="Your name" htmlFor="apply-name" required error={field("name")}>
        <input id="apply-name" className="input" autoComplete="name" maxLength={LIMITS.name} value={a.name} onChange={set("name")} aria-invalid={field("name") ? true : undefined} />
      </Field>
      <Field label="Email address" htmlFor="apply-email" required error={field("email")} hint="We reply here.">
        <input id="apply-email" className="input" type="email" autoComplete="email" maxLength={LIMITS.email} value={a.email} onChange={set("email")} aria-invalid={field("email") ? true : undefined} />
      </Field>
      <Field label="Phone" htmlFor="apply-phone" error={field("phone")}>
        <input id="apply-phone" className="input" type="tel" autoComplete="tel" maxLength={LIMITS.phone} value={a.phone} onChange={set("phone")} aria-invalid={field("phone") ? true : undefined} />
      </Field>
      <Field label="Link to your CV or profile" htmlFor="apply-link" error={field("resume_link")} hint="A web address that starts with https://. We cannot take files here.">
        <input id="apply-link" className="input" type="url" inputMode="url" maxLength={LIMITS.resume_link} value={a.resume_link} onChange={set("resume_link")} aria-invalid={field("resume_link") ? true : undefined} />
      </Field>
      <Field label="A few words about you" htmlFor="apply-note" error={field("cover_letter")}>
        <textarea id="apply-note" className="textarea" rows={5} maxLength={LIMITS.cover_letter} value={a.cover_letter} onChange={set("cover_letter")} aria-invalid={field("cover_letter") ? true : undefined} />
      </Field>
      {/* A field no person sees or reaches: a program that fills it in is not one. */}
      <div className="ppl-honey" aria-hidden="true">
        <label htmlFor="apply-website">Leave this empty</label>
        <input id="apply-website" tabIndex={-1} autoComplete="off" value={a.website} onChange={set("website")} />
      </div>
      <p className="faint">We use what you send only to consider you for this role, and keep it as long as we need to.</p>
      <div>
        <Button type="submit" variant="primary" loading={send.isPending}>
          Send my application
        </Button>
      </div>
    </form>
  );
}

function Detail({ slug, id }: { slug: string; id: string }) {
  const q = useQuery({ queryKey: ["careers", slug, id], queryFn: ({ signal }) => api.get<Opening>(`${base(slug)}/${id}`, undefined, signal) });
  const [sent, setSent] = useState<string | null>(null);
  useShellTitle(q.data ? q.data.title : null);
  if (q.error) {
    const gone = q.error instanceof ApiError && q.error.status === 404;
    return gone ? (
      <EmptyState title="This opening is closed" icon="file">
        <p className="muted">It may have been filled, or taken down.</p>
        <Link to={`/careers/${slug}`}>See the other openings</Link>
      </EmptyState>
    ) : (
      <ErrorState error={q.error} onRetry={() => void q.refetch()} />
    );
  }
  if (!q.data) return <Spinner label="Loading the opening" />;
  const o = q.data;
  return (
    <article className="stack">
      <Link to={`/careers/${slug}`} className="ppl-back">
        All openings
      </Link>
      <header>
        <h1 className="page-title">{o.title}</h1>
        <p className="page-sub">{[o.company, meta(o)].filter(Boolean).join(" · ")}</p>
        {o.closes_on && <p className="faint">Applications close on {formatDate(o.closes_on)}.</p>}
      </header>
      {o.description ? <Markdown source={o.description} /> : <p className="muted">There is no description yet.</p>}
      {sent ? (
        <div className="card card-pad ppl-thanks" role="status">
          <h2 className="ppl-sub">Thank you{sent ? `, ${sent.split(/\s+/)[0]}` : ""}</h2>
          <p>We have your application. If it looks like a fit, we will write to you at the address you gave.</p>
          <Link to={`/careers/${slug}`}>See the other openings</Link>
        </div>
      ) : (
        <div className="card card-pad">
          <Form slug={slug} opening={o} onSent={setSent} />
        </div>
      )}
    </article>
  );
}

function List({ slug }: { slug: string }) {
  const q = useQuery({ queryKey: ["careers", slug], queryFn: ({ signal }) => api.get<{ openings: Opening[] }>(base(slug), undefined, signal) });
  if (q.error) return <ErrorState error={q.error} onRetry={() => void q.refetch()} />;
  if (!q.data) return <Spinner label="Loading the openings" />;
  if (!q.data.openings.length) {
    return (
      <EmptyState title="No openings right now" icon="user">
        <p className="muted">There is nothing to apply for today. Please look again later.</p>
      </EmptyState>
    );
  }
  return (
    <ul className="ppl-openings" data-ferrum-nocard aria-label="Openings">
      {q.data.openings.map((o) => (
        <li key={o.id} className="card card-pad">
          <Link className="ppl-opening-title" to={`/careers/${slug}/${o.id}`}>
            {o.title}
          </Link>
          <p className="muted">{[o.company, meta(o)].filter(Boolean).join(" · ")}</p>
          {o.posted_on && <p className="faint">Posted {formatDate(o.posted_on)}</p>}
        </li>
      ))}
    </ul>
  );
}

export function Careers() {
  const { slug: asked = "", opening } = useParams();
  const here = bootstrap()?.workspace;
  // The address names the workspace; the page was served for the same one.
  const slug = here?.slug || asked;
  useShellTitle(here?.name ? `Careers at ${here.name}` : "Careers");
  return (
    <main className="ppl-careers">
      <header className="ppl-careers-head">
        <p className="faint">{here?.name}</p>
        <h1 className="page-title">Careers</h1>
      </header>
      {opening ? <Detail slug={slug} id={opening} /> : <List slug={slug} />}
    </main>
  );
}
