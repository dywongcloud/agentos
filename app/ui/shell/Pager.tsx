"use client";

// app/ui/shell/Pager.tsx
//
// Reusable client-side pager, extracted from the bespoke controls that used to
// live inside AuditLog / RecentActivityLive. `paginate` slices a list for the
// current page; `<Pager>` renders the prev/next controls. Same look-and-feel
// as before, now shared.

import { pagerStyles as S } from "@/app/ui/shell/styles";

export function paginate<T>(items: T[], page: number, size: number): T[] {
  const start = (page - 1) * size;
  return items.slice(start, start + size);
}

export function totalPages(count: number, size: number): number {
  return Math.max(1, Math.ceil(count / size));
}

export default function Pager({
  page,
  total,
  count,
  pageSize,
  onPage,
}: {
  // Current 1-based page.
  page: number;
  // Total number of pages.
  total: number;
  // Total item count (for the "x–y of N" label). Optional.
  count?: number;
  // Page size (for the label). Optional.
  pageSize?: number;
  onPage: (next: number) => void;
}) {
  if (total <= 1) return null;

  const safePage = Math.min(Math.max(1, page), total);
  const label =
    count != null && pageSize != null
      ? `${(safePage - 1) * pageSize + 1}–${Math.min(safePage * pageSize, count)} of ${count}`
      : null;

  return (
    <div style={S.bar}>
      {label ? <span style={S.info}>{label}</span> : <span />}
      <div style={S.buttons}>
        <button
          type="button"
          onClick={() => onPage(Math.max(1, safePage - 1))}
          disabled={safePage <= 1}
          style={safePage <= 1 ? S.buttonDisabled : S.button}
          aria-label="Previous page"
        >
          ‹ Prev
        </button>
        <span style={S.info}>
          {safePage} / {total}
        </span>
        <button
          type="button"
          onClick={() => onPage(Math.min(total, safePage + 1))}
          disabled={safePage >= total}
          style={safePage >= total ? S.buttonDisabled : S.button}
          aria-label="Next page"
        >
          Next ›
        </button>
      </div>
    </div>
  );
}
