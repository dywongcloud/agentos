// app/ui/RichText.tsx
//
// Shared rich-text rendering for the /ui inspectors (evals, workflows,
// automations). Agent output is markdown-ish text full of ```json fences,
// inline `code`, and raw JSON blobs — rendering it as one pre-wrapped <div>
// is illegible. These components are composable server-rendered pieces:
//
//   <RichText text={...}/>    fenced code blocks (syntax highlighted, JSON
//                             pretty-printed), inline code, auto-detected
//                             raw-JSON paragraphs, plain text
//   <CodeBlock code lang/>    one highlighted block with a language chip
//   <JsonBlock value/>        pretty-print + highlight any JS value
//   <ToolCallLine call/>      "NAME({...})" eval/job tool-call strings —
//                             name chip + collapsible highlighted args
//
// No client JS: highlighting is span-based at render time; long blocks
// collapse with native <details>.

import type { CSSProperties, ReactNode } from "react";

import { diagnoseFailure } from "@/app/lib/failureDiagnosis";

// Fixed dark palette for code (independent of light/dark UI theme — code on
// dark is legible in both).
const C = {
  bg: "#0d1017",
  border: "#1f2430",
  text: "#dbe2ee",
  muted: "#7d8696",
  key: "#7cc4ff",
  string: "#a8d977",
  number: "#f0b35e",
  literal: "#f47067", // true/false/null + keywords
  punct: "#8b95a7",
  comment: "#6a737d",
};

const codeBox: CSSProperties = {
  background: C.bg,
  border: `1px solid ${C.border}`,
  borderRadius: 8,
  padding: "10px 12px",
  margin: "8px 0",
  overflowX: "auto",
  fontSize: 12,
  lineHeight: 1.55,
  fontFamily:
    "ui-monospace, SFMono-Regular, Menlo, Consolas, 'Liberation Mono', monospace",
  color: C.text,
  whiteSpace: "pre",
  position: "relative",
};

const langChip: CSSProperties = {
  position: "absolute",
  top: 6,
  right: 8,
  fontSize: 9,
  fontWeight: 700,
  letterSpacing: 0.6,
  textTransform: "uppercase",
  color: C.muted,
};

// --- JSON highlighting --------------------------------------------------------

const JSON_TOKEN =
  /("(?:[^"\\]|\\.)*")(\s*:)?|\b(?:true|false|null)\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?|[{}[\],:]/g;

function highlightJson(src: string): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  let k = 0;
  for (const m of src.matchAll(JSON_TOKEN)) {
    const idx = m.index ?? 0;
    if (idx > last) out.push(src.slice(last, idx));
    const tok = m[0];
    if (m[1] !== undefined) {
      // string — key if a colon follows
      const isKey = m[2] !== undefined;
      out.push(
        <span key={k++} style={{ color: isKey ? C.key : C.string }}>
          {m[1]}
        </span>
      );
      if (m[2]) out.push(<span key={k++} style={{ color: C.punct }}>{m[2]}</span>);
    } else if (/^(?:true|false|null)$/.test(tok)) {
      out.push(<span key={k++} style={{ color: C.literal }}>{tok}</span>);
    } else if (/^[{}[\],:]$/.test(tok)) {
      out.push(<span key={k++} style={{ color: C.punct }}>{tok}</span>);
    } else {
      out.push(<span key={k++} style={{ color: C.number }}>{tok}</span>);
    }
    last = idx + tok.length;
  }
  if (last < src.length) out.push(src.slice(last));
  return out;
}

// --- generic (non-JSON) highlighting --------------------------------------------

const GENERIC_TOKEN =
  /("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`)|(\/\/[^\n]*|#[^\n]*)|\b(-?\d+(?:\.\d+)?)\b|\b(const|let|var|function|return|if|else|for|while|import|export|from|await|async|new|class|type|interface|true|false|null|undefined|def|lambda|None|True|False)\b/g;

function highlightGeneric(src: string): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  let k = 0;
  for (const m of src.matchAll(GENERIC_TOKEN)) {
    const idx = m.index ?? 0;
    if (idx > last) out.push(src.slice(last, idx));
    const tok = m[0];
    const color = m[1] ? C.string : m[2] ? C.comment : m[3] ? C.number : C.literal;
    out.push(<span key={k++} style={{ color }}>{tok}</span>);
    last = idx + tok.length;
  }
  if (last < src.length) out.push(src.slice(last));
  return out;
}

function tryParseJson(src: string): unknown | undefined {
  const t = src.trim();
  if (!t || (t[0] !== "{" && t[0] !== "[")) return undefined;
  try {
    return JSON.parse(t);
  } catch {
    return undefined;
  }
}

// --- exported components ----------------------------------------------------------

export function CodeBlock({
  code,
  lang,
  collapse,
}: {
  code: string;
  lang?: string;
  // Collapse blocks longer than this many chars behind <details> (0 = never).
  collapse?: number;
}) {
  let body = code.replace(/\n$/, "");
  let effLang = (lang ?? "").toLowerCase();
  const parsed = effLang === "json" || effLang === "" ? tryParseJson(body) : undefined;
  if (parsed !== undefined) {
    body = JSON.stringify(parsed, null, 2);
    effLang = "json";
  }
  const highlighted =
    effLang === "json" ? highlightJson(body) : highlightGeneric(body);
  const block = (
    <pre style={codeBox}>
      {effLang ? <span style={langChip}>{effLang}</span> : null}
      {highlighted}
    </pre>
  );
  if (collapse && body.length > collapse) {
    return (
      <details>
        <summary
          style={{
            cursor: "pointer",
            fontSize: 11,
            color: "var(--muted-foreground)",
            margin: "6px 0",
          }}
        >
          {effLang || "code"} · {body.length.toLocaleString()} chars — expand
        </summary>
        {block}
      </details>
    );
  }
  return block;
}

export function JsonBlock({
  value,
  collapse,
}: {
  value: unknown;
  collapse?: number;
}) {
  let body: string;
  try {
    body = JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    body = String(value);
  }
  return <CodeBlock code={body} lang="json" collapse={collapse} />;
}

// Inline-code aware plain-text segment.
function TextSegment({ text }: { text: string }) {
  const parts = text.split(/(`[^`\n]+`)/g);
  return (
    <>
      {parts.map((p, i) =>
        p.startsWith("`") && p.endsWith("`") && p.length > 2 ? (
          <code
            key={i}
            style={{
              background: "var(--muted)",
              border: "1px solid var(--border)",
              borderRadius: 5,
              padding: "0px 5px",
              fontSize: "0.92em",
              fontFamily:
                "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
            }}
          >
            {p.slice(1, -1)}
          </code>
        ) : (
          p
        )
      )}
    </>
  );
}

const FENCE = /```([a-zA-Z0-9_-]*)\n?([\s\S]*?)(?:```|$)/g;

// Main renderer: fenced code blocks + inline code + auto-detected raw JSON.
export function RichText({
  text,
  collapse = 2200,
}: {
  text: string;
  collapse?: number;
}) {
  if (!text) return null;
  const nodes: ReactNode[] = [];
  let last = 0;
  let k = 0;
  for (const m of text.matchAll(FENCE)) {
    const idx = m.index ?? 0;
    if (idx > last) {
      nodes.push(<TextSegment key={k++} text={text.slice(last, idx)} />);
    }
    nodes.push(
      <CodeBlock key={k++} code={m[2] ?? ""} lang={m[1] || undefined} collapse={collapse} />
    );
    last = idx + m[0].length;
  }
  const tail = text.slice(last);
  if (tail) {
    // A trailing segment that is pure JSON (an agent dumping a raw object)
    // renders as a highlighted block instead of a wall of text.
    if (tryParseJson(tail) !== undefined && tail.trim().length > 40) {
      nodes.push(<CodeBlock key={k++} code={tail} lang="json" collapse={collapse} />);
    } else {
      nodes.push(<TextSegment key={k++} text={tail} />);
    }
  }
  return (
    <div
      style={{
        whiteSpace: "pre-wrap",
        wordBreak: "break-word",
        lineHeight: 1.5,
      }}
    >
      {nodes}
    </div>
  );
}

// "Why it failed & how to fix it" panel for failed evals/jobs/automation runs.
// Pass the raw error/result text; renders nothing when there's no diagnosis.
export function DiagnosisPanel({ errorText }: { errorText: string | null | undefined }) {
  const d = diagnoseFailure(errorText);
  if (!d) return null;
  const kindChip =
    d.kind === "transient"
      ? { label: "transient — retry", bg: "#fef3c7", border: "#fde68a", fg: "#92400e" }
      : d.kind === "config"
        ? { label: "needs setup", bg: "#fee2e2", border: "#fecaca", fg: "#991b1b" }
        : { label: "needs a change", bg: "#e0e7ff", border: "#c7d2fe", fg: "#3730a3" };
  return (
    <div
      style={{
        border: "1px solid var(--border)",
        borderLeft: `3px solid ${kindChip.fg}`,
        borderRadius: 8,
        background: "var(--card)",
        padding: "10px 14px",
        margin: "10px 0",
        fontSize: 13,
        lineHeight: 1.55,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 6 }}>
        <strong style={{ fontSize: 12.5 }}>Why it failed &amp; how to fix it</strong>
        <span
          style={{
            fontSize: 10,
            fontWeight: 700,
            textTransform: "uppercase",
            letterSpacing: 0.5,
            background: kindChip.bg,
            border: `1px solid ${kindChip.border}`,
            color: kindChip.fg,
            borderRadius: 999,
            padding: "1px 8px",
          }}
        >
          {kindChip.label}
        </span>
      </div>
      <div style={{ marginBottom: 6 }}>{d.cause}</div>
      <ol style={{ margin: 0, paddingLeft: 20, color: "var(--muted-foreground)" }}>
        {d.fix.map((step, i) => (
          <li key={i} style={{ marginBottom: 2 }}>
            {step}
          </li>
        ))}
      </ol>
    </div>
  );
}

// Eval/job tool calls are stored as "TOOL_NAME({...json args...})" strings.
// Render the name as a chip and the args as collapsible highlighted JSON.
export function ToolCallLine({ call }: { call: string }) {
  const open = call.indexOf("(");
  const name = open > 0 ? call.slice(0, open) : call;
  const inner = open > 0 ? call.slice(open + 1, call.lastIndexOf(")")) : "";
  const parsed = tryParseJson(inner);
  return (
    <div style={{ marginBottom: 4 }}>
      <code
        style={{
          fontSize: 11.5,
          fontWeight: 600,
          color: "var(--foreground)",
          background: "var(--muted)",
          border: "1px solid var(--border)",
          borderRadius: 6,
          padding: "1px 7px",
          fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
        }}
      >
        {name}
      </code>
      {parsed !== undefined ? (
        <JsonBlock value={parsed} collapse={500} />
      ) : inner ? (
        <span
          style={{
            fontSize: 12,
            color: "var(--muted-foreground)",
            marginLeft: 8,
            wordBreak: "break-word",
          }}
        >
          {inner.length > 300 ? inner.slice(0, 300) + "…" : inner}
        </span>
      ) : null}
    </div>
  );
}
