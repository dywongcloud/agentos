"use client";

// app/ui/IntegrationsTable.tsx
//
// Paginated integrations table for the /ui integrations tab. Client component
// so it can page in-place with the shared <Pager>; styled from the shell
// design-system tokens.

import { useState, type CSSProperties } from "react";
import Link from "next/link";

import { shell } from "@/app/ui/shell/styles";
import Pager, { paginate, totalPages } from "@/app/ui/shell/Pager";
import { toolkitLogo } from "@/app/ui/toolkitLogo";

export type IntegrationRow = {
  slug: string;
  name?: string;
  connected: boolean;
  connectedAccountId?: string;
};

const PAGE_SIZE = 12;

function formatToolkitName(slug: string, name?: string) {
  if (name && name.trim()) return name;
  return slug.replace(/[-_]/g, " ").replace(/\b\w/g, (m) => m.toUpperCase());
}

function initialsFromSlug(slug: string) {
  const parts = slug.split(/[-_\s]+/).filter(Boolean);
  if (parts.length === 0) return "?";
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase();
  return `${parts[0]![0] ?? ""}${parts[1]![0] ?? ""}`.toUpperCase();
}

export default function IntegrationsTable({
  userId,
  toolkits,
  error,
}: {
  userId: string;
  toolkits: IntegrationRow[];
  error?: string | null;
}) {
  const [page, setPage] = useState(1);
  const pages = totalPages(toolkits.length, PAGE_SIZE);
  const safePage = Math.min(page, pages);
  const visible = paginate(toolkits, safePage, PAGE_SIZE);

  return (
    <>
      <div style={shell.tableWrap}>
        <table style={shell.table}>
          <thead>
            <tr>
              <th style={shell.th}>Integration</th>
              <th style={shell.th}>Slug</th>
              <th style={shell.th}>Status</th>
              <th style={shell.th}>Account</th>
              <th style={shell.th}>Action</th>
            </tr>
          </thead>
          <tbody>
            {error ? (
              <tr>
                <td colSpan={5} style={shell.td}>
                  {error}
                </td>
              </tr>
            ) : toolkits.length === 0 ? (
              <tr>
                <td colSpan={5} style={shell.td}>
                  No integrations found.
                </td>
              </tr>
            ) : (
              visible.map((t) => {
                const logo = toolkitLogo(t.slug);
                const name = formatToolkitName(t.slug, t.name);
                return (
                  <tr key={t.slug}>
                    <td style={shell.tdStrong}>
                      <div style={S.cell}>
                        <div style={S.badge}>
                          {logo ? (
                            <img src={logo} alt={name} style={S.logo} />
                          ) : (
                            <span style={S.fallback}>{initialsFromSlug(t.slug)}</span>
                          )}
                        </div>
                        <span>{name}</span>
                      </div>
                    </td>
                    <td style={shell.td}>
                      <code>{t.slug}</code>
                    </td>
                    <td style={shell.td}>{t.connected ? "Connected" : "Pending"}</td>
                    <td style={shell.td}>
                      {t.connectedAccountId ? <code>{t.connectedAccountId}</code> : "—"}
                    </td>
                    <td style={shell.td}>
                      <Link
                        href={`/api/ui/composio/authorize?userId=${encodeURIComponent(
                          userId
                        )}&toolkit=${encodeURIComponent(t.slug)}`}
                        style={shell.tableLink}
                      >
                        {t.connected ? "Reconnect" : "Connect"}
                      </Link>
                    </td>
                  </tr>
                );
              })
            )}
          </tbody>
        </table>
      </div>
      <Pager
        page={safePage}
        total={pages}
        count={toolkits.length}
        pageSize={PAGE_SIZE}
        onPage={setPage}
      />
    </>
  );
}

const S: Record<string, CSSProperties> = {
  cell: { display: "flex", alignItems: "center", gap: 10 },
  badge: {
    width: 26,
    height: 26,
    borderRadius: 7,
    border: "1px solid var(--border)",
    display: "grid",
    placeItems: "center",
    overflow: "hidden",
    flexShrink: 0,
    background: "var(--card)",
  },
  logo: { width: 18, height: 18, objectFit: "contain" },
  fallback: { fontSize: 10, fontWeight: 700, color: "var(--muted-foreground)" },
};
