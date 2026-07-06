"use client";

// app/ui/ThemeToggle.tsx
//
// Small light/dark toggle. Persists choice to localStorage and flips the
// `dark` class on <html>. Pairs with the inline bootstrap in app/layout.tsx
// so the persisted theme is applied before first paint.

import { useEffect, useState } from "react";

type Theme = "light" | "dark";

function readInitial(): Theme {
  if (typeof window === "undefined") return "light";
  try {
    const saved = localStorage.getItem("claw-theme");
    if (saved === "light" || saved === "dark") return saved;
  } catch {
    // ignore
  }
  if (window.matchMedia?.("(prefers-color-scheme: dark)").matches) return "dark";
  return "light";
}

export default function ThemeToggle({ style }: { style?: React.CSSProperties }) {
  const [theme, setTheme] = useState<Theme>("light");

  useEffect(() => {
    setTheme(readInitial());
  }, []);

  function toggle() {
    const next: Theme = theme === "dark" ? "light" : "dark";
    setTheme(next);
    try {
      localStorage.setItem("claw-theme", next);
    } catch {
      // ignore quota / private mode errors
    }
    // Important: set BOTH classes explicitly. Theme.css has a
    // `@media (prefers-color-scheme: dark) { :root:not(.light):not(.dark) { ... } }`
    // OS-preference fallback — without an explicit `.light` here, toggling
    // to light on a dark-OS would just re-trigger the media query and snap
    // straight back to dark (the "stuck in dark" symptom).
    const html = document.documentElement;
    html.classList.remove("dark", "light");
    html.classList.add(next);
  }

  return (
    <button
      type="button"
      onClick={toggle}
      aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} mode`}
      title={`Switch to ${theme === "dark" ? "light" : "dark"} mode`}
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        width: 32,
        height: 32,
        borderRadius: 8,
        border: "1px solid var(--border)",
        background: "var(--card)",
        color: "var(--foreground)",
        cursor: "pointer",
        padding: 0,
        ...style,
      }}
    >
      {theme === "dark" ? (
        // sun (currently dark → click for light)
        <svg
          width="15"
          height="15"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <circle cx="12" cy="12" r="4" />
          <path d="M12 2v2" />
          <path d="M12 20v2" />
          <path d="m4.93 4.93 1.41 1.41" />
          <path d="m17.66 17.66 1.41 1.41" />
          <path d="M2 12h2" />
          <path d="M20 12h2" />
          <path d="m6.34 17.66-1.41 1.41" />
          <path d="m19.07 4.93-1.41 1.41" />
        </svg>
      ) : (
        // moon (currently light → click for dark)
        <svg
          width="15"
          height="15"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
        </svg>
      )}
    </button>
  );
}
