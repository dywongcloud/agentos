// app/ui/shell/TopBar.tsx
//
// The shared top bar: brand logo + workspace name on the left, quick links +
// theme toggle + avatar on the right. Server component (no client state).

import Link from "next/link";
import { shell } from "@/app/ui/shell/styles";
import { navHref } from "@/app/ui/shell/tabs";
import ThemeToggle from "@/app/ui/ThemeToggle";
import DylanClawLogo from "@/app/ui/DylanClawLogo";

function firstLetter(value: string) {
  const clean = value.trim();
  return clean ? clean[0]!.toUpperCase() : "D";
}

export default function TopBar({
  workspaceName,
  userId,
  q,
}: {
  workspaceName: string;
  userId: string;
  q?: string;
}) {
  return (
    <header style={shell.topbar}>
      <div style={shell.topbarLeft}>
        <Link href="/home" style={shell.brandLink} aria-label="DylanClaw">
          <DylanClawLogo height={22} />
        </Link>
        <div style={shell.slash}>/</div>
        <div style={shell.workspaceName}>{workspaceName}</div>
      </div>

      <div style={shell.topbarRight} className="topbar-links">
        <Link href="/home" style={shell.toplink}>
          Home
        </Link>
        <Link href="/docs" style={shell.toplink}>
          Docs
        </Link>
        <Link href={navHref("activity", userId, q)} style={shell.toplink}>
          Activity
        </Link>
        <Link href={navHref("settings", userId, q)} style={shell.toplink}>
          Settings
        </Link>
        <ThemeToggle />
        <div style={shell.avatarCircle}>{firstLetter(workspaceName)}</div>
      </div>
    </header>
  );
}
