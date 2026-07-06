"use client";

// app/ui/VfsBrowser.tsx
//
// Google-Drive-style browser for the tenant's VFS. Backed by
// /api/ui/vfs/list (which unions all sessions for a user) and
// /api/ui/vfs/url (which mints a signed URL the browser can hit
// directly). Two view modes:
//
//   - grid: large card tiles, file/folder icon up top, name + meta below
//   - list: a compact table with name, size, modified
//
// Folder tiles drill into the path; file tiles open the signed URL in a
// new tab.

import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
} from "react";

type DirEntry = {
  name: string;
  path: string;
  childCount: number;
  lastModified?: string;
};

type FileEntry = {
  name: string;
  path: string;
  size: number;
  mimeType: string;
  lastModified: string;
  sessionId: string;
};

type ListResponse = {
  ok: boolean;
  path: string;
  breadcrumbs: { name: string; path: string }[];
  dirs: DirEntry[];
  files: FileEntry[];
  empty?: boolean;
  reason?: string;
  error?: string;
};

type ViewMode = "grid" | "list";

const ROOT = "/workspace";

export default function VfsBrowser({
  userId,
  initialView = "grid",
}: {
  userId: string;
  initialView?: ViewMode;
}) {
  const [path, setPath] = useState<string>(ROOT);
  const [view, setView] = useState<ViewMode>(initialView);
  const [data, setData] = useState<ListResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  const fetchList = useCallback(
    async (p: string) => {
      setLoading(true);
      setError(null);
      try {
        const qs = new URLSearchParams({ userId, path: p });
        const res = await fetch(`/api/ui/vfs/list?${qs.toString()}`, {
          credentials: "same-origin",
          cache: "no-store",
        });
        const json = (await res.json()) as ListResponse;
        if (!json.ok) {
          setError(json.error ?? json.reason ?? "Failed to load files");
          setData(null);
        } else {
          setData(json);
        }
      } catch (e: any) {
        setError(e?.message ?? String(e));
        setData(null);
      } finally {
        setLoading(false);
      }
    },
    [userId]
  );

  useEffect(() => {
    fetchList(path);
  }, [path, fetchList]);

  const openFile = useCallback(
    async (file: FileEntry) => {
      try {
        const qs = new URLSearchParams({
          userId,
          sessionId: file.sessionId,
          path: file.path,
        });
        const res = await fetch(`/api/ui/vfs/url?${qs.toString()}`, {
          credentials: "same-origin",
          cache: "no-store",
        });
        const json = await res.json();
        if (json?.url) {
          window.open(json.url, "_blank", "noopener,noreferrer");
        }
      } catch {
        // swallow — error already surfaces via list view
      }
    },
    [userId]
  );

  const filtered = useMemo(() => {
    if (!data) return { dirs: [] as DirEntry[], files: [] as FileEntry[] };
    const q = query.trim().toLowerCase();
    if (!q) return { dirs: data.dirs, files: data.files };
    return {
      dirs: data.dirs.filter((d) => d.name.toLowerCase().includes(q)),
      files: data.files.filter((f) => f.name.toLowerCase().includes(q)),
    };
  }, [data, query]);

  return (
    <div>
      <div style={styles.toolbar}>
        <div style={styles.crumbWrap}>
          <button
            type="button"
            onClick={() => setPath(ROOT)}
            style={{
              ...styles.crumb,
              fontWeight: path === ROOT ? 600 : 500,
            }}
          >
            workspace
          </button>
          {(data?.breadcrumbs ?? [])
            .filter((b) => b.path !== ROOT)
            .map((b, i, arr) => {
              const isLast = i === arr.length - 1;
              return (
                <span key={b.path} style={styles.crumbRow}>
                  <span style={styles.crumbSep}>/</span>
                  <button
                    type="button"
                    onClick={() => setPath(b.path)}
                    style={{
                      ...styles.crumb,
                      fontWeight: isLast ? 600 : 500,
                    }}
                  >
                    {b.name}
                  </button>
                </span>
              );
            })}
        </div>

        <div style={styles.toolbarRight}>
          <div style={styles.searchWrap}>
            <span style={styles.searchIcon}>⌕</span>
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search in this folder"
              style={styles.searchInput}
            />
          </div>

          <button
            type="button"
            onClick={() => fetchList(path)}
            style={styles.iconBtn}
            aria-label="Refresh"
            title="Refresh"
          >
            ↻
          </button>

          <div style={styles.viewToggle}>
            <button
              type="button"
              onClick={() => setView("grid")}
              style={view === "grid" ? styles.viewBtnActive : styles.viewBtn}
              aria-pressed={view === "grid"}
              title="Grid view"
            >
              ▦
            </button>
            <button
              type="button"
              onClick={() => setView("list")}
              style={view === "list" ? styles.viewBtnActive : styles.viewBtn}
              aria-pressed={view === "list"}
              title="List view"
            >
              ☰
            </button>
          </div>
        </div>
      </div>

      {loading && !data ? (
        <div style={styles.empty}>Loading…</div>
      ) : error ? (
        <div style={styles.errorBox}>{error}</div>
      ) : !filtered.dirs.length && !filtered.files.length ? (
        <div style={styles.empty}>
          {query
            ? "No items match your search."
            : "No files in this folder yet. Files appear here once an agent writes to your VFS."}
        </div>
      ) : view === "grid" ? (
        <GridView
          dirs={filtered.dirs}
          files={filtered.files}
          onOpenDir={setPath}
          onOpenFile={openFile}
        />
      ) : (
        <ListView
          dirs={filtered.dirs}
          files={filtered.files}
          onOpenDir={setPath}
          onOpenFile={openFile}
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Grid view
// ---------------------------------------------------------------------------

function GridView({
  dirs,
  files,
  onOpenDir,
  onOpenFile,
}: {
  dirs: DirEntry[];
  files: FileEntry[];
  onOpenDir: (path: string) => void;
  onOpenFile: (file: FileEntry) => void;
}) {
  return (
    <div>
      {dirs.length > 0 && (
        <>
          <div style={styles.sectionLabel}>Folders</div>
          <div style={styles.grid}>
            {dirs.map((d) => (
              <button
                key={d.path}
                type="button"
                onClick={() => onOpenDir(d.path)}
                style={styles.tile}
              >
                <div style={styles.tileThumb}>
                  <FolderIcon />
                </div>
                <div style={styles.tileBody}>
                  <div style={styles.tileName} title={d.name}>
                    {d.name}
                  </div>
                  <div style={styles.tileMeta}>
                    {d.childCount} item{d.childCount === 1 ? "" : "s"}
                    {d.lastModified ? ` · ${relTime(d.lastModified)}` : ""}
                  </div>
                </div>
              </button>
            ))}
          </div>
        </>
      )}

      {files.length > 0 && (
        <>
          <div style={{ ...styles.sectionLabel, marginTop: dirs.length ? 24 : 0 }}>
            Files
          </div>
          <div style={styles.grid}>
            {files.map((f) => (
              <button
                key={`${f.sessionId}:${f.path}`}
                type="button"
                onClick={() => onOpenFile(f)}
                style={styles.tile}
              >
                <div style={styles.tileThumb}>
                  <FileIcon name={f.name} />
                </div>
                <div style={styles.tileBody}>
                  <div style={styles.tileName} title={f.name}>
                    {f.name}
                  </div>
                  <div style={styles.tileMeta}>
                    {humanSize(f.size)} · {relTime(f.lastModified)}
                  </div>
                </div>
              </button>
            ))}
          </div>
        </>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// List view
// ---------------------------------------------------------------------------

function ListView({
  dirs,
  files,
  onOpenDir,
  onOpenFile,
}: {
  dirs: DirEntry[];
  files: FileEntry[];
  onOpenDir: (path: string) => void;
  onOpenFile: (file: FileEntry) => void;
}) {
  return (
    <div style={styles.listBox}>
      <div style={styles.listHeader}>
        <div style={styles.listColName}>Name</div>
        <div style={styles.listColSize}>Size</div>
        <div style={styles.listColMod}>Last modified</div>
      </div>
      {dirs.map((d) => (
        <button
          key={d.path}
          type="button"
          onClick={() => onOpenDir(d.path)}
          style={styles.listRow}
        >
          <div style={styles.listColName}>
            <span style={styles.listIcon}>
              <FolderIcon small />
            </span>
            <span style={styles.listName}>{d.name}</span>
          </div>
          <div style={styles.listColSize}>
            {d.childCount} item{d.childCount === 1 ? "" : "s"}
          </div>
          <div style={styles.listColMod}>
            {d.lastModified ? relTime(d.lastModified) : "—"}
          </div>
        </button>
      ))}
      {files.map((f) => (
        <button
          key={`${f.sessionId}:${f.path}`}
          type="button"
          onClick={() => onOpenFile(f)}
          style={styles.listRow}
        >
          <div style={styles.listColName}>
            <span style={styles.listIcon}>
              <FileIcon name={f.name} small />
            </span>
            <span style={styles.listName}>{f.name}</span>
          </div>
          <div style={styles.listColSize}>{humanSize(f.size)}</div>
          <div style={styles.listColMod}>{relTime(f.lastModified)}</div>
        </button>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

function FolderIcon({ small = false }: { small?: boolean }) {
  const size = small ? 18 : 40;
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M3 7.5A2.5 2.5 0 0 1 5.5 5h3.382a2 2 0 0 1 1.414.586l1.618 1.618A2 2 0 0 0 13.328 7.8H18.5A2.5 2.5 0 0 1 21 10.3v7.2A2.5 2.5 0 0 1 18.5 20h-13A2.5 2.5 0 0 1 3 17.5v-10Z" />
    </svg>
  );
}

function FileIcon({ name, small = false }: { name: string; small?: boolean }) {
  const ext = (name.split(".").pop() ?? "").toLowerCase();
  const palette = pickPalette(ext);
  const size = small ? 18 : 40;
  const tag = ext.slice(0, 4);
  return (
    <div
      style={{
        position: "relative",
        width: size,
        height: size,
        color: palette.fg,
      }}
    >
      <svg
        viewBox="0 0 24 24"
        width={size}
        height={size}
        fill="none"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
      >
        <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8l-5-5Z" />
        <path d="M14 3v5h5" />
      </svg>
      {!small && tag ? (
        <div
          style={{
            position: "absolute",
            left: 0,
            right: 0,
            top: "60%",
            textAlign: "center",
            fontSize: 8,
            fontWeight: 700,
            letterSpacing: "0.06em",
            color: palette.fg,
            textTransform: "uppercase",
          }}
        >
          {tag}
        </div>
      ) : null}
    </div>
  );
}

function pickPalette(ext: string): { fg: string; bg: string } {
  const map: Record<string, { fg: string; bg: string }> = {
    md: { fg: "#3b82f6", bg: "#dbeafe" },
    txt: { fg: "#64748b", bg: "#e2e8f0" },
    json: { fg: "#f59e0b", bg: "#fef3c7" },
    csv: { fg: "#10b981", bg: "#d1fae5" },
    js: { fg: "#eab308", bg: "#fef9c3" },
    ts: { fg: "#0ea5e9", bg: "#e0f2fe" },
    py: { fg: "#22c55e", bg: "#dcfce7" },
    sh: { fg: "#a78bfa", bg: "#ede9fe" },
    html: { fg: "#f97316", bg: "#ffedd5" },
    png: { fg: "#ec4899", bg: "#fce7f3" },
    jpg: { fg: "#ec4899", bg: "#fce7f3" },
    jpeg: { fg: "#ec4899", bg: "#fce7f3" },
    pdf: { fg: "#ef4444", bg: "#fee2e2" },
  };
  return map[ext] ?? { fg: "var(--muted-foreground)", bg: "var(--muted)" };
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function humanSize(bytes: number): string {
  if (!bytes) return "0 B";
  const u = ["B", "KB", "MB", "GB"];
  let n = bytes;
  let i = 0;
  while (n >= 1024 && i < u.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(n < 10 && i > 0 ? 1 : 0)} ${u[i]}`;
}

function relTime(iso: string): string {
  const t = Date.parse(iso);
  if (!t) return "—";
  const diff = Date.now() - t;
  const s = Math.round(diff / 1000);
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 48) return `${h}h ago`;
  const d = Math.round(h / 24);
  if (d < 14) return `${d}d ago`;
  return new Date(t).toLocaleDateString();
}

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

const styles: Record<string, CSSProperties> = {
  toolbar: {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 12,
    marginBottom: 18,
    flexWrap: "wrap",
  },
  crumbWrap: {
    display: "flex",
    alignItems: "center",
    flexWrap: "wrap",
    gap: 0,
    minWidth: 0,
    color: "var(--foreground)",
  },
  crumbRow: {
    display: "inline-flex",
    alignItems: "center",
  },
  crumb: {
    background: "transparent",
    border: 0,
    color: "var(--foreground)",
    fontSize: 14,
    cursor: "pointer",
    padding: "4px 8px",
    borderRadius: 6,
  },
  crumbSep: {
    color: "var(--muted-foreground)",
    padding: "0 2px",
  },
  toolbarRight: {
    display: "flex",
    alignItems: "center",
    gap: 8,
  },
  searchWrap: {
    display: "flex",
    alignItems: "center",
    border: "1px solid var(--border)",
    borderRadius: 9,
    height: 34,
    padding: "0 10px",
    background: "var(--card)",
    minWidth: 220,
  },
  searchIcon: {
    color: "var(--muted-foreground)",
    fontSize: 14,
    marginRight: 6,
  },
  searchInput: {
    border: 0,
    outline: "none",
    background: "transparent",
    color: "var(--foreground)",
    fontSize: 12,
    flex: 1,
  },
  iconBtn: {
    height: 34,
    width: 34,
    borderRadius: 9,
    border: "1px solid var(--border)",
    background: "var(--card)",
    color: "var(--foreground)",
    cursor: "pointer",
    fontSize: 14,
  },
  viewToggle: {
    display: "inline-flex",
    border: "1px solid var(--border)",
    background: "var(--muted)",
    borderRadius: 9,
    padding: 2,
    gap: 2,
  },
  viewBtn: {
    height: 30,
    width: 36,
    borderRadius: 7,
    border: 0,
    background: "transparent",
    color: "var(--muted-foreground)",
    cursor: "pointer",
    fontSize: 14,
  },
  viewBtnActive: {
    height: 30,
    width: 36,
    borderRadius: 7,
    border: 0,
    background: "var(--card)",
    color: "var(--foreground)",
    cursor: "pointer",
    fontSize: 14,
    boxShadow: "0 1px 2px rgba(0,0,0,0.18)",
  },
  sectionLabel: {
    fontSize: 11,
    fontWeight: 700,
    letterSpacing: "0.06em",
    textTransform: "uppercase",
    color: "var(--muted-foreground)",
    margin: "0 0 12px 0",
  },
  grid: {
    display: "grid",
    gridTemplateColumns: "repeat(auto-fill, minmax(180px, 1fr))",
    gap: 14,
  },
  tile: {
    display: "flex",
    flexDirection: "column",
    alignItems: "stretch",
    gap: 12,
    padding: 14,
    border: "1px solid var(--border)",
    borderRadius: 12,
    background: "var(--card)",
    cursor: "pointer",
    textAlign: "left",
    color: "var(--foreground)",
    transition: "border-color 120ms ease, transform 120ms ease",
    minHeight: 130,
  },
  tileThumb: {
    flex: 1,
    borderRadius: 9,
    background: "var(--muted)",
    color: "var(--muted-foreground)",
    display: "grid",
    placeItems: "center",
    minHeight: 70,
  },
  tileBody: {
    display: "flex",
    flexDirection: "column",
    gap: 2,
    minWidth: 0,
  },
  tileName: {
    fontSize: 13,
    fontWeight: 600,
    color: "var(--foreground)",
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  tileMeta: {
    fontSize: 11,
    color: "var(--muted-foreground)",
  },
  listBox: {
    border: "1px solid var(--border)",
    borderRadius: 12,
    background: "var(--card)",
    overflow: "hidden",
  },
  listHeader: {
    display: "grid",
    gridTemplateColumns: "1fr 120px 180px",
    padding: "10px 14px",
    background: "var(--muted)",
    color: "var(--muted-foreground)",
    fontSize: 10,
    fontWeight: 700,
    letterSpacing: "0.06em",
    textTransform: "uppercase",
    borderBottom: "1px solid var(--border)",
  },
  listRow: {
    display: "grid",
    gridTemplateColumns: "1fr 120px 180px",
    padding: "12px 14px",
    alignItems: "center",
    background: "var(--card)",
    color: "var(--foreground)",
    border: 0,
    borderBottom: "1px solid var(--border)",
    cursor: "pointer",
    textAlign: "left",
    fontSize: 13,
    width: "100%",
  },
  listColName: {
    display: "flex",
    alignItems: "center",
    gap: 10,
    minWidth: 0,
  },
  listColSize: {
    color: "var(--muted-foreground)",
    fontSize: 12,
  },
  listColMod: {
    color: "var(--muted-foreground)",
    fontSize: 12,
  },
  listIcon: {
    color: "var(--muted-foreground)",
    display: "inline-flex",
    flexShrink: 0,
  },
  listName: {
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  empty: {
    padding: "44px 24px",
    border: "1px dashed var(--border)",
    borderRadius: 12,
    background: "var(--card)",
    color: "var(--muted-foreground)",
    textAlign: "center",
    fontSize: 13,
  },
  errorBox: {
    padding: "16px 18px",
    border: "1px solid var(--destructive)",
    borderRadius: 12,
    background: "var(--card)",
    color: "var(--destructive)",
    fontSize: 13,
  },
};
