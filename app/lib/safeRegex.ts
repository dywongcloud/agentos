// app/lib/safeRegex.ts
//
// Bounded evaluation of USER-SUPPLIED regexes (chat-automation trigger
// patterns). These run on the inbound hot path against every message; Node's
// regex engine is backtracking and single-threaded, so a catastrophic pattern
// like (a+)+ hangs the shared serverless instance for ALL tenants — a
// cross-tenant availability failure, not an O(1) match.
//
// This is a MITIGATION, not a proof of ReDoS-freedom (deciding that statically
// is undecidable in general). Two concrete bounds:
//   1. Reject the classic nested-quantifier shape — a group containing an
//      unbounded quantifier that is itself quantified — and over-long patterns.
//   2. Cap the input length fed to .test(), bounding worst-case work per call.

const MAX_INPUT = 4000;
const MAX_PATTERN = 1000;

// A parenthesised group that contains a `+`/`*` and is itself followed by a
// `+`/`*`/`{` — i.e. (a+)+ , (a*)* , (.*)+ , (ab+)* , (x+){2,} . This is the
// dominant catastrophic-backtracking family.
const NESTED_QUANTIFIER = /\([^()]*[+*][^()]*\)\s*[+*{]/;

export function isDangerousRegex(pattern: string): boolean {
  return pattern.length > MAX_PATTERN || NESTED_QUANTIFIER.test(pattern);
}

// Compile a user pattern, or null if it is invalid OR flagged dangerous.
export function safeCompileRegex(pattern: string, flags = "i"): RegExp | null {
  if (isDangerousRegex(pattern)) return null;
  try {
    return new RegExp(pattern, flags);
  } catch {
    return null;
  }
}

// Test with a length-capped input so a single call's work is bounded.
export function safeRegexTest(re: RegExp, text: string): boolean {
  return re.test(text.length > MAX_INPUT ? text.slice(0, MAX_INPUT) : text);
}
