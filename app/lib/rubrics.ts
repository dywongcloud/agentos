// app/lib/rubrics.ts
//
// Modality detection + critic rubrics. The verifier loads one of these
// rubrics based on the planner's `kind` classification (or a heuristic
// fallback) and uses it to decide pass vs revise.
//
// The point of having modality-specific rubrics is that "good output" means
// very different things for ZK circuits vs LaTeX PDFs vs Next.js UI code, and
// a generic "is this useful?" prompt will let half-assed work pass.
//
// Add a new modality:
//   1) push its id into ModalityId
//   2) add a rubric below
//   3) (optional) add detection hints in detectModality()

export type ModalityId =
  | "code-rust"
  | "code-rust-zk"
  | "code-ui-nextjs-ts"
  | "code-generic"
  | "latex-pdf"
  | "research"
  | "generic";

export type Rubric = {
  id: ModalityId;
  // Short label used in logs / Telegram echoes.
  label: string;
  // The criteria block — concatenated into the verifier prompt.
  criteria: string[];
  // Forbidden patterns — if the result contains any of these, it
  // auto-fails before even calling the model. Cheap pre-filter.
  hardFails: RegExp[];
};

const BASE_HARD_FAILS: RegExp[] = [
  // "skeleton" / "stub" / "TODO" / placeholder language are dead giveaways of
  // half-assed output across every modality.
  /\bTODO\b|\bFIXME\b|placeholder|skeleton/i,
  /lorem ipsum/i,
];

export const RUBRICS: Record<ModalityId, Rubric> = {
  "code-rust": {
    id: "code-rust",
    label: "Rust code",
    hardFails: [...BASE_HARD_FAILS, /unimplemented!\s*\(/i, /todo!\s*\(/i],
    criteria: [
      "Compiles under stable Rust without warnings (the writer should have mentally type-checked).",
      "No `unwrap()` on user input paths; all `Result` and `Option` are handled with `?`, `match`, or explicit messages.",
      "Idiomatic ownership: borrows where possible, owned only when required, no needless clones.",
      "Public APIs have rustdoc comments only when non-obvious; no docstrings that just restate the function name.",
      "Includes `Cargo.toml` snippet with exact dependency versions when crates are used.",
      "No magic numbers without a constant + comment for the magic.",
    ],
  },

  "code-rust-zk": {
    id: "code-rust-zk",
    label: "Rust ZK SNARK circuit",
    hardFails: [...BASE_HARD_FAILS, /unimplemented!\s*\(/i, /todo!\s*\(/i],
    criteria: [
      "Identifies the proving system (Groth16 / PLONK / Halo2 / Nova / etc.) and justifies it.",
      "Constraints are explicit; gadget composition is sound (no underconstrained inputs).",
      "Field arithmetic uses the prime field of the chosen proving system, not native ints.",
      "Witness vs. public-input split is clearly delineated and matches the threat model.",
      "Includes a soundness sanity check: at least one negative test that should fail to verify.",
      "Setup phase (trusted setup vs. transparent) is documented and matches the proving system.",
      "If using arkworks / halo2_proofs / circom / noir, version-pinned dependencies are listed.",
    ],
  },

  "code-ui-nextjs-ts": {
    id: "code-ui-nextjs-ts",
    label: "Next.js TypeScript UI",
    hardFails: [
      ...BASE_HARD_FAILS,
      // `: any` is the smoke that gives away a half-finished UI.
      /:\s*any\b/,
      /@ts-ignore/,
      /@ts-expect-error/,
    ],
    criteria: [
      "Every prop and hook has a precise type — no `any`, no `unknown` as escape hatch.",
      "Server vs client components are explicitly marked (`'use client'` only where needed).",
      "Accessibility: interactive elements have aria attributes, semantic HTML where possible.",
      "Loading / error states are handled, not silently ignored.",
      "Styling is consistent (Tailwind utility ordering, or styled-components/CSS modules — pick one and stick with it).",
      "Data fetching uses Next.js conventions (Server Components, route handlers, or `use` + Suspense).",
      "No `useEffect` for derived state that React can compute synchronously.",
    ],
  },

  "code-generic": {
    id: "code-generic",
    label: "general code",
    hardFails: BASE_HARD_FAILS,
    criteria: [
      "Compiles / runs in the stated language and runtime.",
      "Error paths are handled, not swallowed.",
      "No dead code, no commented-out blocks, no debug prints left in.",
      "Function-level documentation only where the WHY is non-obvious; no narration comments.",
      "Imports / dependencies are real and pinnable.",
    ],
  },

  "latex-pdf": {
    id: "latex-pdf",
    label: "LaTeX long-form document",
    hardFails: [
      ...BASE_HARD_FAILS,
      /\\todo\{/i,
      /\\missing/i,
    ],
    criteria: [
      "Compiles under pdflatex or xelatex with the declared packages.",
      "Document length matches the request (e.g. 50–100 pages when asked).",
      "Has sectioning consistent with technical writing: title, toc, abstract, sections, bibliography.",
      "Uses BibTeX/biblatex for citations; no inline `(Author, 2023)` fakery.",
      "Figures, tables, and equations are referenced via `\\ref{}`, not page numbers.",
      "Margins, font, and line spacing are set explicitly (no defaults that look amateurish).",
      "Mathematical notation is consistent (one convention for vectors, sets, operators).",
    ],
  },

  research: {
    id: "research",
    label: "research / deep dive",
    hardFails: BASE_HARD_FAILS,
    criteria: [
      "Answers the user's actual question, not an adjacent one.",
      "Claims are backed by sources; sources are real URLs and named correctly.",
      "Multiple angles are considered, not just the first plausible answer.",
      "Disagreements / open questions in the literature are acknowledged, not ignored.",
      "Includes a 'what I'm uncertain about' section if the topic warrants it.",
      "Length matches depth requested — short questions get short answers, deep research gets long writeups.",
    ],
  },

  generic: {
    id: "generic",
    label: "generic task",
    hardFails: BASE_HARD_FAILS,
    criteria: [
      "Directly addresses the user's request without padding.",
      "No placeholders, no 'I would do X' instead of doing X.",
      "Includes whatever scaffolding the task needs (files, examples, references).",
    ],
  },
};

// Quick heuristic modality classifier used as a fallback when the planner
// doesn't emit a kind. The planner can override this.
export function detectModality(prompt: string): ModalityId {
  const t = (prompt ?? "").toLowerCase();
  const has = (re: RegExp) => re.test(t);

  if (has(/\bzk[- ]?(snark|stark|circuit|plonk|halo2|groth16)\b/) && has(/\brust\b/)) {
    return "code-rust-zk";
  }
  if (has(/\bzk[- ]?(snark|stark|circuit)\b/)) return "code-rust-zk";
  if (has(/\brust\b/)) return "code-rust";
  if (has(/\bnext\.?js\b/) && has(/\btypescript\b|\bts\b|\btsx\b/)) {
    return "code-ui-nextjs-ts";
  }
  if (has(/\blatex\b/) || has(/\bpdf\b/) && has(/\b(\d{2,3})[\s-]*(?:page|pp)\b/)) {
    return "latex-pdf";
  }
  if (has(/\b(deep\s*research|research|literature|state[- ]of[- ]the[- ]art|survey)\b/)) {
    return "research";
  }
  if (has(/\bcode\b|\bimplement\b|\bfunction\b|\bclass\b/)) return "code-generic";
  return "generic";
}

export function rubricFor(id: ModalityId): Rubric {
  return RUBRICS[id] ?? RUBRICS.generic;
}

// Pre-filter: if any hardFail regex matches the result text, the verifier
// should auto-revise without even spending a model call.
export function findHardFails(rubric: Rubric, text: string): string[] {
  const out: string[] = [];
  for (const re of rubric.hardFails) {
    if (re.test(text)) out.push(`hard-fail: matched /${re.source}/${re.flags}`);
  }
  return out;
}
