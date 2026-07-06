// app/ui/shell/TabBar.tsx
//
// The shared pill-style tab bar. Highlights the `active` tab and links to each
// surface via navHref. Horizontally scrollable on narrow screens — the
// scrollbar is hidden by the .tabbar-scroll rule in responsive.css.

import Link from "next/link";
import { shell } from "@/app/ui/shell/styles";
import { TOP_TABS, navHref, tabLabel, type TabKey } from "@/app/ui/shell/tabs";

export default function TabBar({
  active,
  userId,
  q,
}: {
  active: TabKey;
  userId: string;
  q?: string;
}) {
  return (
    <nav style={shell.tabbar} className="tabbar-scroll" aria-label="Sections">
      {TOP_TABS.map((tab) => (
        <Link
          key={tab}
          href={navHref(tab, userId, q)}
          style={tab === active ? shell.mainTabActive : shell.mainTab}
          aria-current={tab === active ? "page" : undefined}
        >
          {tabLabel(tab)}
        </Link>
      ))}
    </nav>
  );
}
