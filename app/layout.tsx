import type { Metadata } from "next";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";

import { bootstrapDev } from "@/app/bootstrap";

import "@/app/ui/theme.css";
import "@/app/ui/responsive.css";

export const metadata: Metadata = {
  title: "DylanClaw",
  description: "Gateway + Workflow DevKit autonomous bot runtime",
};

// Inline script: reads the persisted theme before first paint and sets the
// `dark` class on <html> so there's no light → dark flash on reload.
const themeBootstrap = `
(function () {
  try {
    var html = document.documentElement;

    // When iframed into the WDK dashboard (the Agents/Evals/Activity/Logs
    // tabs render /ui/* with ?embed=1), follow the PARENT dashboard's theme
    // instead of our own localStorage. The dashboard uses next-themes under a
    // different storage key ("workflow-theme"), so without this each tab would
    // resolve its own light/dark and drift out of sync with the chrome around
    // it. Same-origin, so the parent's <html> class is readable; a
    // MutationObserver keeps us locked to the dashboard's live toggle.
    if (window.self !== window.top) {
      var sync = function () {
        try {
          var pe = window.parent.document.documentElement;
          // next-themes may flip either a class or a data-theme attribute
          // depending on its config; honor both so we never miss a toggle.
          var dark =
            pe.classList.contains("dark") ||
            pe.getAttribute("data-theme") === "dark";
          html.classList.remove("dark", "light");
          html.classList.add(dark ? "dark" : "light");
        } catch (e) {}
      };
      sync();
      try {
        new MutationObserver(sync).observe(
          window.parent.document.documentElement,
          { attributes: true, attributeFilter: ["class", "data-theme"] }
        );
      } catch (e) {}
      return;
    }

    var saved = localStorage.getItem("claw-theme");
    // Mirror ThemeToggle's convention: set BOTH .dark and .light explicitly,
    // never just one. That way the @media(prefers-color-scheme: dark)
    // fallback in theme.css (gated on :root:not(.light):not(.dark)) only
    // fires when the operator has made no explicit choice, and toggling
    // off dark sticks even on a dark-mode OS.
    if (saved === "dark" || saved === "light") {
      html.classList.add(saved);
    } else if (window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches) {
      // No saved choice → follow OS; the media query handles it on its own.
    }
  } catch (e) {}
})();
`.trim();

export default async function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  await bootstrapDev();
  return (
    <html
      lang="en"
      className={`${GeistSans.variable} ${GeistMono.variable}`}
      suppressHydrationWarning
    >
      <head>
        <script dangerouslySetInnerHTML={{ __html: themeBootstrap }} />
      </head>
      <body>{children}</body>
    </html>
  );
}
