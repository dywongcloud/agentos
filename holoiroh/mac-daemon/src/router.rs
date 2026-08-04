//! Per-prompt intent classifier choosing between H Company's two hosted `holo3` models.
//!
//! ## Why this exists
//!
//! `holo3-1-35b-a3b` (fast, cheap, 4k max output) is what `holo serve` uses by default. The
//! only shipped desktop agent config pins this model (see `holo_bridge::process`'s module
//! doc). For complex, multi-step tasks, a bigger model does better: `holo3-122b-a10b` (32k
//! max output, ~1.6x the price).
//!
//! Both ids are real. Both ids are currently live on H Company's hosted gateway
//! (`GET https://api.hcompany.ai/v1/models`, verified 2026-07-20). Both ids are `is_active`
//! and `is_ready`. `holo3-1-35b-a3b` supports `["reasoning","tools"]`. `holo3-122b-a10b`
//! supports `["reasoning"]` only (see this module's "122b caveat" note below).
//!
//! Model selection is a SPAWN-TIME knob on `holo serve` (`--model` or
//! `HAI_AGENT_RUNTIME_MODEL` -- see `holo_bridge::process::HoloServeProcess::build_command`).
//! Model selection is not a per-request parameter. `agent_client/requests.py` in the
//! installed CLI sets `model=None`, with the literal comment "spawn-time
//! HAI_AGENT_RUNTIME_MODEL wins, no per-request override". So switching models means a full
//! terminate and respawn of `holo serve`. This reuses the exact same process-swap machinery
//! that the tinfoil rate-limit failover already uses (`HoloBridge::switch_to`; see
//! [`crate::holo_bridge::HoloBridge::route_model`]).
//!
//! ## 122b caveat
//!
//! The gateway does not list `"tools"` under `holo3-122b-a10b`'s `supported_features`. The
//! only shipped desktop agent prompt and config is 35B-tuned. The runtime binary does carry
//! real 122b support: a `build_holo_3_122b_a10b_localizer` factory exists, and a
//! `MODEL_DEFAULTS` entry exists. So routing to `holo3-122b-a10b` is expected to work. This
//! route stays untested end-to-end with a real paid completion, because a real test would
//! cost money to run here. If complex-tier turns come back malformed or tool-call-free, check
//! this route first.
//!
//! ## Hysteresis
//!
//! Every tier change costs a real respawn: terminate, spawn, health wait, and agent-card
//! probe, typically a few seconds total. [`should_switch`] avoids thrashing on a queue of
//! alternating light and heavy prompts. [`should_switch`] only approves a switch when the new
//! classification is DECISIVE relative to the currently active tier (see its own doc).

/// This is the model routing tier. `Tier::Simple` (`holo3-1-35b-a3b`) is the always-safe
/// default. Every path that cannot confidently classify a prompt treats the prompt as
/// `Simple`. Every path that has no prior tier to compare against also treats the prompt as
/// `Simple`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Simple,
    Complex,
}

impl Tier {
    /// Returns the real H Company hosted model id for this tier. This module verified
    /// both ids live against the hosted models gateway on 2026-07-20 (see this module's
    /// doc). This module did not guess these ids.
    pub fn model_id(self) -> &'static str {
        match self {
            Tier::Simple => "holo3-1-35b-a3b",
            Tier::Complex => "holo3-122b-a10b",
        }
    }

    /// Returns the tier that requests `model_id`, if `model_id` names one of the two
    /// known models. This function recovers "what tier are we currently on" from
    /// `HoloBridge`'s stored `routed_model` string. This function avoids a redundant
    /// parallel enum field.
    pub fn from_model_id(model_id: &str) -> Option<Tier> {
        match model_id {
            "holo3-1-35b-a3b" => Some(Tier::Simple),
            "holo3-122b-a10b" => Some(Tier::Complex),
            _ => None,
        }
    }
}

/// This is the additive complexity score threshold. `score >= COMPLEX_THRESHOLD` classifies
/// the prompt as [`Tier::Complex`]. See [`classify`]'s heuristics.
const COMPLEX_THRESHOLD: i32 = 4;

/// This is a hysteresis threshold for [`should_switch`]. Switching FROM [`Tier::Simple`]
/// requires a score at or above this threshold. This threshold demands stronger evidence
/// than the bare classify threshold. This stops a borderline prompt, right after a simple
/// streak, from triggering a respawn.
const UPGRADE_THRESHOLD: i32 = 5;

/// Switching FROM [`Tier::Complex`] back down requires a score at or below this threshold
/// (a near-empty complexity score). This stops a merely-average prompt, in the middle of a
/// complex streak, from bouncing the model back down.
const DOWNGRADE_THRESHOLD: i32 = 1;

/// These are the recognized app or target names a prompt might reference. Cross-app prompts
/// that mention two or more of these tend to need more planning than single-app prompts.
/// This list is seeded from the daemon's own skill-catalog naming, plus common aliases. This
/// module deliberately uses plain-text substring matching, with no external dependency (see
/// this module's doc on why routing stays dependency-free).
const KNOWN_APPS: &[&str] = &[
    "calendar",
    "contacts",
    "mail",
    "email",
    "messages",
    "imessage",
    "text",
    "notes",
    "reminders",
    "system settings",
    "chrome",
    "safari",
    "browser",
    "discord",
    "finder",
    "notion",
    "slack",
    "spotify",
    "music",
    "photos",
    "terminal",
    "preview",
    "pages",
    "numbers",
    "keynote",
    "xcode",
];

/// These sequencing or connective words signal a multi-step plan when present.
const SEQUENCING_WORDS: &[&str] = &[
    "then",
    "after",
    "next",
    "once",
    "before",
    "finally",
    "afterward",
    "afterwards",
];

/// These conditional or branching words stress a smaller model's planning.
const CONDITIONAL_WORDS: &[&str] = &["if", "unless", "otherwise", "depending", "whichever"];

/// These are known imperative action verbs. This module counts these verbs at clause
/// starts, as a proxy for "how many distinct actions does this prompt actually ask for".
const ACTION_VERBS: &[&str] = &[
    "open",
    "click",
    "type",
    "search",
    "send",
    "reply",
    "create",
    "delete",
    "move",
    "copy",
    "drag",
    "compose",
    "book",
    "schedule",
    "download",
    "install",
    "compare",
    "summarize",
    "find",
    "fill",
    "upload",
    "organize",
    "rename",
    "close",
    "switch",
    "toggle",
    "scroll",
];

/// Classify a prompt's raw text into a routing tier by an additive, dependency-free heuristic
/// score. See this module's doc for the design rationale: why per-prompt classification is
/// spawn-cost-bound, and why hysteresis exists on top of this. [`should_switch`] re-derives
/// the raw score via [`score`]. [`should_switch`] does not work off this function's collapsed
/// `Tier`.
///
/// Heuristics (see [`score`]'s inline comments for exact weights):
/// - word/char length,
/// - sequencing connectives ("then", "after", ...),
/// - numbered/bulleted step lists,
/// - distinct app/target names mentioned,
/// - conjunction density,
/// - distinct imperative-verb count,
/// - conditional/branching words,
/// - a short-circuit fast path for obviously trivial prompts ("open safari").
#[allow(dead_code)] // documented public entry point; `should_switch` re-scores directly for routing
pub fn classify(prompt: &str) -> Tier {
    if score(prompt) >= COMPLEX_THRESHOLD {
        Tier::Complex
    } else {
        Tier::Simple
    }
}

/// Returns the raw additive complexity score behind [`classify`]. This function is
/// exposed separately so [`should_switch`] can apply its own tighter hysteresis thresholds.
/// This avoids re-running or duplicating the scoring logic.
fn score(prompt: &str) -> i32 {
    let trimmed = prompt.trim();
    let lower = trimmed.to_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();

    // Fast path: a short, single-verb, no-connective prompt is unambiguously simple --
    // skip the rest of the scoring (e.g. "open safari", "pause spotify", "volume up").
    if words.len() <= 6
        && !SEQUENCING_WORDS.iter().any(|w| lower.contains(w))
        && !lower.contains(" and ")
        && !lower.contains(';')
        && !has_list_markers(trimmed)
    {
        return 0;
    }

    let mut total: i32 = 0;

    // Length.
    if words.len() > 50 {
        total += 3;
    } else if words.len() > 25 || trimmed.len() > 180 {
        total += 2;
    }

    // Sequencing connectives (word-boundary-safe via split_whitespace tokens plus a couple of
    // two-word phrases checked as substrings).
    let sequencing_count = words
        .iter()
        .filter(|w| SEQUENCING_WORDS.contains(&trim_punct(w)))
        .count()
        + lower.matches("and then").count()
        + lower.matches("after that").count();
    if sequencing_count >= 4 {
        total += 3;
    } else if sequencing_count >= 2 {
        total += 2;
    }

    // Numbered/bulleted step lists.
    if has_list_markers(trimmed) {
        total += 3;
    }

    // Distinct app/target names.
    let app_count = KNOWN_APPS.iter().filter(|app| lower.contains(*app)).count();
    if app_count >= 2 {
        total += 2;
    }

    // Conjunction density.
    let conjunction_count = lower.matches(" and ").count()
        + lower.matches("; ").count()
        + lower.matches(", then").count();
    if conjunction_count >= 4 {
        total += 2;
    } else if conjunction_count >= 2 {
        total += 1;
    }

    // Distinct imperative verbs present anywhere (a cheap proxy for "how many actions").
    let verb_count = ACTION_VERBS.iter().filter(|v| words.contains(v)).count();
    if verb_count >= 3 {
        total += 2;
    }

    // Explicit compound/cross-referencing patterns.
    if lower.contains("for each")
        || lower.contains("every ")
        || lower.contains("all of the")
        || lower.contains("all the")
        || lower.contains("compare ")
        || lower.contains("cross-reference")
        || lower.contains("cross reference")
        || lower.contains("research ")
    {
        total += 2;
    }

    // Conditionals / branching.
    if words
        .iter()
        .any(|w| CONDITIONAL_WORDS.contains(&trim_punct(w)))
    {
        total += 2;
    }

    total
}

/// Decides whether `prompt` should switch `holo serve` away from the currently active
/// `active` tier. This function applies decisive hysteresis, so a queue of alternating
/// light and heavy prompts does not thrash the model on every turn. `Some(new_tier)` means
/// switch, and names the tier to switch to. `None` means stay on `active`.
///
/// This function scores `prompt` once, via the crate-private [`score`]. This function
/// checks that score against the tighter [`UPGRADE_THRESHOLD`] and [`DOWNGRADE_THRESHOLD`]
/// bounds. This check is the single real source of truth for "should we actually respawn,
/// and onto what". So the returned tier can never disagree with the decision that produced
/// it. This differs from deciding via [`classify`]'s separately-thresholded,
/// already-collapsed verdict.
pub fn should_switch(active: Tier, prompt: &str) -> Option<Tier> {
    let s = score(prompt);
    match active {
        Tier::Simple if s >= UPGRADE_THRESHOLD => Some(Tier::Complex),
        Tier::Complex if s <= DOWNGRADE_THRESHOLD => Some(Tier::Simple),
        _ => None,
    }
}

fn has_list_markers(prompt: &str) -> bool {
    let mut marker_lines = 0;
    for line in prompt.lines() {
        let t = line.trim_start();
        let looks_numbered = t
            .split_once(['.', ')'])
            .map(|(head, _)| !head.is_empty() && head.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false);
        let looks_bulleted = t.starts_with("- ") || t.starts_with("* ");
        let looks_step = t.to_lowercase().starts_with("step ");
        if looks_numbered || looks_bulleted || looks_step {
            marker_lines += 1;
        }
    }
    marker_lines >= 2
}

fn trim_punct(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric())
}
