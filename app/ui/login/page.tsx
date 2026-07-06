// app/ui/login/page.tsx
//
// Sign-in screen for the DylanClaw dashboard. Visually consistent with /home and
// /docs — Geist sans, theme tokens, gold accent + radial glow.

import Link from "next/link";

import "./login.css";

export const dynamic = "force-dynamic";
export const metadata = {
  title: "Sign in — DylanClaw",
};

export default async function LoginPage({
  searchParams,
}: {
  searchParams?: Promise<{ error?: string; next?: string }>;
}) {
  const sp = (await searchParams) ?? {};
  const error = sp.error === "1";
  const next = sp.next?.startsWith("/") ? sp.next : null;

  return (
    <main className="l-page">
      <nav className="l-nav">
        <Link href="/home" className="l-brand">
          <span className="l-brand-mark">D</span>
          DylanClaw
        </Link>
        <Link href="/docs" className="l-nav-link">
          Docs
        </Link>
      </nav>

      <div className="l-shell">
        <section className="l-card" aria-labelledby="l-title">
          <div className="l-card-head">
            <div className="l-card-mark" aria-hidden="true">
              D
            </div>
            <h1 id="l-title" className="l-title">
              Sign in to DylanClaw
            </h1>
            <p className="l-sub">
              Restricted to the workspace owner. Use the password configured
              as <code style={{ fontFamily: "var(--font-mono)" }}>ADMIN_UI_PASSWORD</code>.
            </p>
          </div>

          {error ? (
            <div className="l-error" role="alert">
              That password didn&apos;t match. Try again.
            </div>
          ) : null}

          <form action="/api/ui/login" method="post" className="l-form">
            {next ? (
              <input type="hidden" name="next" value={next} />
            ) : null}
            <div className="l-field">
              <label htmlFor="l-password" className="l-label">
                Password
              </label>
              <input
                id="l-password"
                className="l-input"
                type="password"
                name="password"
                autoFocus
                autoComplete="current-password"
                placeholder="••••••••"
                required
              />
            </div>
            <button type="submit" className="l-submit">
              Sign in
            </button>
          </form>

          <div className="l-foot">
            <span>
              No account?{" "}
              <Link href="/home" className="l-foot-link">
                Back to home
              </Link>
            </span>
            <Link href="/docs" className="l-foot-link">
              View docs
            </Link>
          </div>
        </section>
      </div>
    </main>
  );
}
