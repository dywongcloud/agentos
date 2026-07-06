// app/ui/shell/AppShell.tsx
//
// The one chrome every /ui surface wraps its body in: top bar + tab bar +
// (optional) hero header + a centered, fluid-padded content section. Server
// component. Pages pass `active` to drive tab highlighting and supply their
// own body as children.

import type { ReactNode } from "react";
import { shell } from "@/app/ui/shell/styles";
import { type TabKey } from "@/app/ui/shell/tabs";
import TopBar from "@/app/ui/shell/TopBar";
import TabBar from "@/app/ui/shell/TabBar";

function firstLetter(value: string) {
  const clean = value.trim();
  return clean ? clean[0]!.toUpperCase() : "D";
}

export default function AppShell({
  active,
  userId,
  workspaceName,
  title,
  subtitle,
  heroActions,
  showHero = true,
  q,
  children,
}: {
  active: TabKey;
  userId: string;
  workspaceName: string;
  title?: string;
  subtitle?: ReactNode;
  heroActions?: ReactNode;
  showHero?: boolean;
  q?: string;
  children: ReactNode;
}) {
  return (
    <main style={shell.page}>
      <div style={shell.app}>
        <TopBar workspaceName={workspaceName} userId={userId} q={q} />
        <TabBar active={active} userId={userId} q={q} />

        <section style={shell.content}>
          {showHero && (title || subtitle || heroActions) && (
            <div style={shell.heroRow} className="hero-row">
              <div style={shell.heroLeft}>
                <div style={shell.bigAvatar}>{firstLetter(workspaceName)}</div>
                <div>
                  {title && <h1 style={shell.pageTitle}>{title}</h1>}
                  {subtitle && <div style={shell.subline}>{subtitle}</div>}
                </div>
              </div>
              {heroActions && <div>{heroActions}</div>}
            </div>
          )}
          {children}
        </section>
      </div>
    </main>
  );
}
