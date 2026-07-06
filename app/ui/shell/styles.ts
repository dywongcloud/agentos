// app/ui/shell/styles.ts
//
// Shared design-system styles for every /ui surface. Extracted from the
// inline `styles` object that used to live only in page.tsx so the chrome
// (top bar, tab bar, hero, cards, tables, forms) is identical everywhere.
//
// Responsiveness is expressed inline where possible: fluid spacing via
// clamp(), reflowing grids via repeat(auto-fit, minmax()). The few things
// inline styles can't do (scrollbar hiding, true media-query collapses) live
// in app/ui/responsive.css.

import type { CSSProperties } from "react";

export const shell: Record<string, CSSProperties> = {
  page: {
    minHeight: "100vh",
    background: "var(--background)",
    padding: 0,
    margin: 0,
    color: "var(--foreground)",
    fontFamily:
      'Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif',
  },

  app: {
    minHeight: "100vh",
    background: "var(--card)",
  },

  topbar: {
    height: 62,
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    // Fluid side padding: hugs the screen on mobile, matches the old 93px on
    // wide screens.
    padding: "0 clamp(16px, 5vw, 93px)",
    borderBottom: "1px solid var(--border)",
    gap: 12,
    flexWrap: "wrap",
  },

  topbarLeft: {
    display: "flex",
    alignItems: "center",
    gap: 12,
  },

  brandLink: {
    color: "var(--foreground)",
    display: "inline-flex",
    alignItems: "center",
  },

  slash: {
    color: "var(--muted-foreground)",
    fontSize: 30,
    lineHeight: 1,
    fontWeight: 200,
    marginTop: -3,
  },

  workspaceName: {
    fontSize: 25,
    fontWeight: 500,
    lineHeight: 1,
    letterSpacing: "-0.02em",
  },

  topbarRight: {
    display: "flex",
    alignItems: "center",
    gap: 14,
    marginLeft: "auto",
  },

  toplink: {
    textDecoration: "none",
    color: "var(--foreground)",
    fontSize: 11,
    fontWeight: 500,
  },

  avatarCircle: {
    width: 29,
    height: 29,
    borderRadius: 999,
    background: "var(--primary)",
    color: "var(--card)",
    display: "grid",
    placeItems: "center",
    fontSize: 15,
    fontWeight: 500,
  },

  // Pill-style tab bar. A muted background holds the whole row; the active tab
  // gets an elevated card background. Scrolls horizontally on narrow screens
  // (className "tabbar-scroll" in responsive.css hides the scrollbar).
  tabbar: {
    display: "flex",
    alignItems: "center",
    gap: 4,
    margin: "16px clamp(16px, 5vw, 93px) 0 clamp(16px, 5vw, 93px)",
    padding: 4,
    background: "var(--muted)",
    borderRadius: 10,
    width: "fit-content",
    maxWidth: "calc(100% - clamp(16px, 5vw, 93px) * 2)",
    overflowX: "auto",
  },

  mainTab: {
    textDecoration: "none",
    color: "var(--muted-foreground)",
    fontSize: 13,
    fontWeight: 500,
    padding: "6px 14px",
    borderRadius: 7,
    whiteSpace: "nowrap",
    transition: "color 120ms ease, background 120ms ease",
  },

  mainTabActive: {
    textDecoration: "none",
    color: "var(--foreground)",
    fontSize: 13,
    fontWeight: 600,
    padding: "6px 14px",
    borderRadius: 7,
    background: "var(--card)",
    boxShadow: "0 1px 2px 0 rgba(0,0,0,0.18)",
    whiteSpace: "nowrap",
  },

  content: {
    // Fluid horizontal padding + centered max-width so wide screens don't
    // sprawl and narrow screens don't clip. Old fixed value was 114px.
    padding: "29px clamp(16px, 6vw, 114px) 45px clamp(16px, 6vw, 114px)",
    maxWidth: 1480,
    margin: "0 auto",
  },

  heroRow: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 15,
    marginBottom: 26,
    flexWrap: "wrap",
  },

  heroLeft: {
    display: "flex",
    alignItems: "center",
    gap: 15,
  },

  bigAvatar: {
    width: 57,
    height: 57,
    borderRadius: 999,
    background: "var(--primary)",
    color: "var(--card)",
    display: "grid",
    placeItems: "center",
    fontSize: 36,
    fontWeight: 400,
    flexShrink: 0,
  },

  pageTitle: {
    margin: 0,
    fontSize: 23,
    lineHeight: 1.15,
    letterSpacing: "-0.03em",
    fontWeight: 650,
  },

  subline: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    marginTop: 6,
    color: "var(--muted-foreground)",
    fontSize: 11,
  },

  // Shared cards / panels / sections.
  panelCard: {
    border: "1px solid var(--border)",
    borderRadius: 14,
    background: "var(--card)",
    boxShadow: "0 2px 6px rgba(0,0,0,0.04)",
    padding: 18,
  },

  sectionTitle: {
    margin: "0 0 11px 0",
    fontSize: 17,
    fontWeight: 700,
    color: "var(--foreground)",
  },

  sectionText: {
    margin: 0,
    color: "var(--muted-foreground)",
    fontSize: 11,
    lineHeight: 1.6,
  },

  // Tables.
  tableWrap: {
    overflowX: "auto",
  },

  table: {
    width: "100%",
    borderCollapse: "collapse",
  },

  th: {
    textAlign: "left",
    padding: "9px 9px",
    fontSize: 9,
    color: "var(--muted-foreground)",
    fontWeight: 600,
    borderBottom: "1px solid var(--border)",
    whiteSpace: "nowrap",
  },

  td: {
    padding: "11px 9px",
    fontSize: 11,
    color: "var(--foreground)",
    borderBottom: "1px solid var(--border)",
    whiteSpace: "nowrap",
    verticalAlign: "middle",
  },

  tdStrong: {
    padding: "11px 9px",
    fontSize: 11,
    color: "var(--foreground)",
    borderBottom: "1px solid var(--border)",
    whiteSpace: "nowrap",
    verticalAlign: "middle",
    fontWeight: 600,
  },

  tableLink: {
    textDecoration: "none",
    color: "var(--status-running)",
    fontWeight: 600,
  },

  // Forms / inputs / buttons.
  input: {
    width: "100%",
    height: 33,
    borderRadius: 9,
    border: "1px solid var(--border)",
    padding: "0 9px",
    outline: "none",
    fontSize: 11,
    color: "var(--foreground)",
    background: "var(--card)",
  },

  submitButton: {
    height: 33,
    borderRadius: 9,
    border: 0,
    background: "var(--foreground)",
    color: "var(--card)",
    fontSize: 11,
    fontWeight: 600,
    cursor: "pointer",
    padding: "0 11px",
  },

  submitButtonAlt: {
    height: 33,
    borderRadius: 9,
    border: "1px solid var(--border)",
    background: "var(--card)",
    color: "var(--foreground)",
    fontSize: 11,
    fontWeight: 600,
    cursor: "pointer",
    padding: "0 11px",
  },
};

// Pager controls, shared by AuditLog / RecentActivityLive / dashboard lists.
export const pagerStyles: Record<string, CSSProperties> = {
  bar: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
    marginTop: 12,
    flexWrap: "wrap",
  },
  info: {
    color: "var(--muted-foreground)",
    fontSize: 11,
  },
  buttons: {
    display: "flex",
    alignItems: "center",
    gap: 6,
  },
  button: {
    height: 28,
    minWidth: 28,
    padding: "0 10px",
    borderRadius: 8,
    border: "1px solid var(--border)",
    background: "var(--card)",
    color: "var(--foreground)",
    fontSize: 11,
    fontWeight: 600,
    cursor: "pointer",
  },
  buttonDisabled: {
    height: 28,
    minWidth: 28,
    padding: "0 10px",
    borderRadius: 8,
    border: "1px solid var(--border)",
    background: "var(--muted)",
    color: "var(--muted-foreground)",
    fontSize: 11,
    fontWeight: 600,
    cursor: "not-allowed",
    opacity: 0.6,
  },
};
