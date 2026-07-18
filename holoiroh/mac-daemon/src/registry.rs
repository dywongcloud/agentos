//! Local app registry (Project Aro PRD §8, "target-resolution pipeline",
//! row P0-4): a user-editable mapping from spoken destination aliases to
//! **deterministic launch routes**, plus alias resolution and the
//! deterministic-launch step that runs *before* any visual automation.
//!
//! ## Where this sits in PRD §8's pipeline
//!
//! PRD §8 defines target resolution as a numbered pipeline. Steps 1-4 are
//! the **deterministic** front half this module implements:
//!
//! 1. Take the spoken destination (e.g. "open Slack", "go to Slack").
//! 2. Resolve it against this registry ([`Registry::resolve`]).
//! 3. If exactly one [`RegistryEntry`] matches, that is the target -- no
//!    guessing, no vision. If several match, the PRD is explicit that the
//!    system must **ask the user which one** (a `ambiguous_choice`
//!    `input_request`, see `PROTOCOL.md`), never silently pick one; this
//!    module surfaces that as [`Resolution::Ambiguous`] carrying every
//!    candidate so the caller can build that choice prompt. If none match,
//!    [`Resolution::NotFound`].
//! 4. For a resolved `native_app` entry, **launch it deterministically**
//!    via `open -b <bundle_id>` ([`RegistryEntry::launch_command`] /
//!    [`RegistryEntry::launch`]) -- a stable macOS API call keyed on a
//!    bundle ID, not a screenshot-and-click.
//!
//! Only **step 5+** (locating and operating UI *inside* the launched app,
//! where the target is visually ambiguous) hands off to the computer-use
//! executor. Nothing in this module invokes that executor: this is the
//! deterministic half, and its whole point is to shrink how often the
//! non-deterministic vision path is needed at all.
//!
//! ## What this module is *not* (yet)
//!
//! Same honest posture as `sensitive_categories.rs`: this is the data
//! model, config-file persistence, resolution logic, and the real
//! `open -b` launch primitive. It is **not** wired into a live voice/prompt
//! path -- nothing in `main.rs`/`control_channel.rs`/`holo_bridge` calls
//! [`Registry::resolve`] or [`RegistryEntry::launch`] on a real spoken
//! destination today. The daemon still forwards prompts straight through to
//! `holo serve`. Wiring this in requires a spoken-destination extraction
//! step (turning a transcript into a candidate destination string) and a
//! place in the turn lifecycle to run the deterministic route before the
//! executor -- neither exists in this crate yet. This module makes the
//! deterministic route *possible and testable*; it does not yet make the
//! live turn use it.
//!
//! ## Encryption status: PLAINTEXT with a documented TODO -- NOT encrypted
//!
//! PRD §8 calls for the registry to be **encrypted at rest**. This pass
//! does **not** implement encryption. The registry is written and read as
//! plaintext TOML/JSON at `~/.holoiroh/registry.{toml,json}`, exactly like
//! `sensitive_categories.rs`. This is called out loudly rather than
//! silently skipped:
//!
//! > **TODO(encryption, PRD §8):** persist this file encrypted at rest
//! > (e.g. a macOS Keychain-held key wrapping an AEAD-encrypted blob) and
//! > decrypt on load. Until then `~/.holoiroh/registry.*` is human-readable
//! > plaintext -- do not put secrets in it; aliases/bundle IDs/URLs are not
//! > secret, but the file location and format are not to be mistaken for
//! > the encrypted-at-rest store the PRD ultimately requires.
//!
//! No function in this module claims to encrypt anything, and none does.
//! The load/save path is byte-for-byte the plaintext pattern from
//! `sensitive_categories.rs`, deliberately, so the encryption TODO is a
//! single well-marked seam rather than scattered.
//!
//! ## Why `#![allow(dead_code)]`
//!
//! Nothing in `main.rs` calls into this module yet (see "What this module
//! is not" above). It is registered via `mod registry;` so it compiles and
//! is reachable for a future live target-resolution row, and so
//! `examples/registry_probe.rs` can exercise it for real. Every item here
//! is real, working, documented public API -- this module-level attribute
//! just avoids repeating `#[allow(dead_code)]` on each not-yet-called item,
//! same convention as `sensitive_categories.rs`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// What kind of deterministic route an entry resolves to, per PRD §8's
/// example schema (`entry_type`).
///
/// - [`EntryType::NativeApp`]: a locally-installed macOS application,
///   launched deterministically by bundle ID (`open -b <bundle_id>`). This
///   is the P0 verified-tier shape (Slack, see [`Registry::default_registry`]).
/// - [`EntryType::BrowserUrl`]: a destination that is really a URL to open
///   in a browser. Modeled here for schema completeness per the PRD;
///   deterministic *launch* for this variant is intentionally **not**
///   implemented in this pass (see [`RegistryEntry::launch_command`]), since
///   the alpha's verified example is the native-app path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryType {
    NativeApp,
    BrowserUrl,
}

/// Per-entry action policy, matching PRD §8's example `policy` object.
///
/// This is carried through resolution so a caller has the entry's policy in
/// hand at the moment it decides what to do -- it is **not enforced** by
/// this module (there is no live executor here to enforce against; same
/// gap as `sensitive_categories.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    /// The actions allowed at this destination (PRD §8 example:
    /// `["send_message", "read"]`). Free-form action identifiers here; a
    /// real executor-side allowlist would define the closed vocabulary.
    #[serde(default)]
    pub allowed_actions: Vec<String>,
    /// PRD §8 `remote_view_required`: whether operating this destination
    /// requires the iPhone-side remote view to be active (e.g. because the
    /// user must watch a sensitive action). Defaults to `false`.
    #[serde(default)]
    pub remote_view_required: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            allowed_actions: Vec::new(),
            remote_view_required: false,
        }
    }
}

/// Per-entry defaults, matching PRD §8's example `defaults` object. Kept as
/// its own struct (rather than inlining `workspace`) so the PRD's
/// `defaults: { workspace }` shape round-trips on disk exactly, and so more
/// default fields (channel, account, ...) can be added later without
/// changing the entry's top-level shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Defaults {
    /// Default workspace to assume for this destination when the spoken
    /// request doesn't name one (PRD §8 example: a Slack workspace). `None`
    /// when the destination has no workspace concept or no default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// One registry entry: a set of spoken aliases mapping to a single
/// deterministic route, matching PRD §8's example schema
/// (`alias`, `entry_type`, `bundle_id`, `defaults`, `policy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// The spoken destination aliases that resolve to this entry (PRD §8
    /// `alias`), e.g. `["slack", "slack app", "team chat"]`. Matching is
    /// case-insensitive and whitespace-normalized (see
    /// [`Registry::resolve`]); store them however reads naturally.
    pub alias: Vec<String>,
    /// Whether this is a native app or a browser URL (PRD §8 `entry_type`).
    pub entry_type: EntryType,
    /// macOS bundle ID for [`EntryType::NativeApp`] entries (PRD §8
    /// `bundle_id`), e.g. `com.tinyspeck.slackmacgap`. Required for a
    /// native-app launch; `None` (and unused) for browser-URL entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    /// The URL to open for [`EntryType::BrowserUrl`] entries. `None` for
    /// native-app entries. (Not in PRD §8's *native-app* example object,
    /// but required to make the browser-URL `entry_type` a real, non-empty
    /// route rather than a bare tag.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_url: Option<String>,
    /// Per-entry defaults (PRD §8 `defaults`, e.g. `{ workspace }`).
    #[serde(default)]
    pub defaults: Defaults,
    /// Per-entry action policy (PRD §8 `policy`, e.g.
    /// `{ allowed_actions, remote_view_required }`).
    #[serde(default)]
    pub policy: Policy,
}

impl RegistryEntry {
    /// Builds the deterministic launch [`Command`] for this entry **without
    /// running it** -- the `open -b <bundle_id>` invocation (PRD §8 step 4).
    ///
    /// Returned as a not-yet-spawned [`Command`] on purpose: it lets a
    /// caller (or a probe) inspect/print exactly what would run before
    /// deciding to actually launch, and keeps the "construct the route" and
    /// "run the route" concerns separate.
    ///
    /// Errors (rather than launching the wrong thing) if this entry is not
    /// a launchable native app:
    /// - a [`EntryType::BrowserUrl`] entry has no `open -b` route (browser
    ///   launch is deliberately out of scope for this pass -- see
    ///   [`EntryType`]); or
    /// - a [`EntryType::NativeApp`] entry with no `bundle_id` (a malformed
    ///   hand-edited config) cannot be launched by bundle ID.
    pub fn launch_command(&self) -> Result<Command> {
        match self.entry_type {
            EntryType::NativeApp => {
                let bundle_id = self.bundle_id.as_deref().with_context(|| {
                    format!(
                        "registry entry {:?} is a native_app but has no bundle_id, so it cannot be launched by `open -b`",
                        self.alias
                    )
                })?;
                let mut cmd = Command::new("open");
                cmd.arg("-b").arg(bundle_id);
                Ok(cmd)
            }
            EntryType::BrowserUrl => bail!(
                "registry entry {:?} is a browser_url, which has no deterministic `open -b` native-app launch (browser launch is out of scope for this pass -- see EntryType docs)",
                self.alias
            ),
        }
    }

    /// **Actually runs** the deterministic launch (PRD §8 step 4): builds
    /// the [`Self::launch_command`] and spawns it, opening the app.
    ///
    /// On macOS `open -b <bundle_id>` exits 0 immediately once it has asked
    /// LaunchServices to open (or foreground) the app; a non-zero exit
    /// (e.g. the bundle ID isn't installed) is surfaced as an error here.
    /// This blocks only for the brief lifetime of the `open` helper itself,
    /// not for the launched app.
    ///
    /// This is the deterministic route and runs **before** any visual
    /// automation -- see the module doc. It never touches the computer-use
    /// executor.
    pub fn launch(&self) -> Result<()> {
        let mut cmd = self.launch_command()?;
        let status = cmd
            .status()
            .with_context(|| format!("spawning `open -b` for registry entry {:?}", self.alias))?;
        if !status.success() {
            bail!(
                "`open -b` for registry entry {:?} exited with {} (is the app installed?)",
                self.alias,
                status
            );
        }
        Ok(())
    }
}

/// The full registry, as loaded from (or defaulted for) the user-editable
/// config file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub entries: Vec<RegistryEntry>,
}

/// The outcome of resolving a spoken destination against the registry
/// (PRD §8 steps 2-3).
///
/// The three variants are exhaustive and force the caller to handle
/// ambiguity explicitly -- the PRD's core requirement is that ambiguity
/// **must** become a user choice, never an autonomous guess, so there is
/// deliberately no "best match" or "first match" convenience that would let
/// a caller collapse [`Resolution::Ambiguous`] into a silent pick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// Exactly one entry matched -- the deterministic target.
    Single(&'a RegistryEntry),
    /// More than one entry matched. Per PRD §8 this **must** produce a user
    /// choice prompt (a `ambiguous_choice` `input_request`, see
    /// `PROTOCOL.md`), never an autonomous guess. Carries every candidate
    /// so the caller can present them all.
    Ambiguous(Vec<&'a RegistryEntry>),
    /// No entry matched the spoken destination.
    NotFound,
}

/// Which on-disk format to read/write. Same two-format support and
/// TOML-first default as `sensitive_categories.rs`'s `ConfigFormat`: TOML
/// is the friendlier format for a human to hand-edit, JSON is offered for
/// tooling that prefers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Toml,
    Json,
}

impl ConfigFormat {
    /// Infers the format from a path's extension, defaulting to
    /// [`ConfigFormat::Toml`] for anything other than a recognized `.json`
    /// (including no extension), matching the TOML-first default path. Same
    /// rule as `sensitive_categories::ConfigFormat::from_path`.
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("json") => ConfigFormat::Json,
            _ => ConfigFormat::Toml,
        }
    }
}

/// Normalizes an alias / spoken destination for matching: trims, lowercases
/// (ASCII), and collapses internal whitespace runs to a single space, so
/// `"  Slack   App "` and `"slack app"` compare equal. Kept private and
/// applied to *both* sides of every comparison in [`Registry::resolve`] so
/// the two can never drift.
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .map(|w| w.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Registry {
    /// Default config file location: `~/.holoiroh/registry.toml`. Resolved
    /// via `$HOME`, same approach as
    /// `SensitiveCategories::default_path`/`Allowlist::default_path` -- this
    /// daemon is macOS-only, where `$HOME` is always set for an interactive
    /// login session.
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home).join(".holoiroh").join("registry.toml"))
    }

    /// The alternate JSON config file location, for callers that prefer
    /// `.json` over the default `.toml`: `~/.holoiroh/registry.json`.
    pub fn default_json_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home).join(".holoiroh").join("registry.json"))
    }

    /// The built-in default registry. Alpha scope per PRD §8: a single
    /// Slack `native_app` entry, the P0 **verified-tier** example
    /// (`bundle_id = com.tinyspeck.slackmacgap`). This is a seed a real
    /// deployment is expected to extend via the config file, not a finished
    /// registry -- exactly why the file is user-editable.
    ///
    /// The Slack entry mirrors PRD §8's example schema field-for-field: a
    /// small set of spoken aliases, `entry_type = native_app`, the verified
    /// bundle ID, a `defaults.workspace`, and a `policy` with
    /// `allowed_actions` + `remote_view_required`.
    pub fn default_registry() -> Self {
        Registry {
            entries: vec![RegistryEntry {
                alias: vec![
                    "slack".to_string(),
                    "slack app".to_string(),
                    "team chat".to_string(),
                ],
                entry_type: EntryType::NativeApp,
                bundle_id: Some("com.tinyspeck.slackmacgap".to_string()),
                browser_url: None,
                defaults: Defaults {
                    // Illustrative default workspace; a real deployment edits this.
                    workspace: Some("primary".to_string()),
                },
                policy: Policy {
                    allowed_actions: vec!["send_message".to_string(), "read".to_string()],
                    remote_view_required: false,
                },
            }],
        }
    }

    /// Loads the registry from `path`, in the format inferred by
    /// [`ConfigFormat::from_path`]. A missing file is **not** an error --
    /// it's the expected first-run state, and this returns
    /// [`Self::default_registry`] then (call [`Self::save`] to persist it,
    /// or use [`Self::load_or_init`] which does that for you). Any other I/O
    /// error, or a parse failure, is a real error -- a corrupt registry
    /// silently treated as "just use defaults" would discard a user's
    /// hand-added destinations, which is exactly the kind of silent config
    /// loss this file exists to avoid.
    ///
    /// PLAINTEXT read -- see the module doc's encryption TODO. This does not
    /// decrypt anything because nothing writes an encrypted file yet.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default_registry());
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("reading registry file at {}", path.display()));
            }
        };

        match ConfigFormat::from_path(path) {
            ConfigFormat::Toml => {
                let text = String::from_utf8(bytes).with_context(|| {
                    format!("registry file at {} is not valid UTF-8", path.display())
                })?;
                toml::from_str(&text)
                    .with_context(|| format!("parsing registry TOML at {}", path.display()))
            }
            ConfigFormat::Json => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing registry JSON at {}", path.display())),
        }
    }

    /// Convenience wrapper around [`Self::load`] using [`Self::default_path`].
    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path()?)
    }

    /// Loads from `path` if it exists; otherwise builds
    /// [`Self::default_registry`] **and persists it** to `path` so the file
    /// exists (with the alpha Slack seed) after the first run, ready for the
    /// user to hand-edit. Returns the loaded or newly-defaulted-and-saved
    /// value either way.
    pub fn load_or_init(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            return Self::load(path);
        }
        let defaults = Self::default_registry();
        defaults
            .save(path)
            .with_context(|| format!("writing default registry file at {}", path.display()))?;
        Ok(defaults)
    }

    /// Convenience wrapper around [`Self::load_or_init`] using
    /// [`Self::default_path`] (i.e. `~/.holoiroh/registry.toml`).
    pub fn load_or_init_default() -> Result<Self> {
        Self::load_or_init(Self::default_path()?)
    }

    /// Writes `self` to `path`, in the format inferred by
    /// [`ConfigFormat::from_path`], creating the parent directory
    /// (`~/.holoiroh/`) if needed. Overwrites the whole file, same
    /// one-writer-at-a-time posture as `SensitiveCategories::save` /
    /// `Allowlist::save`.
    ///
    /// PLAINTEXT write -- see the module doc's encryption TODO. This writes
    /// human-readable TOML/JSON on purpose (for now); it does **not**
    /// encrypt, and does not pretend to.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating registry directory {}", parent.display()))?;
        }

        match ConfigFormat::from_path(path) {
            ConfigFormat::Toml => {
                let text = toml::to_string_pretty(self).context("serializing registry to TOML")?;
                std::fs::write(path, text)
            }
            ConfigFormat::Json => {
                let json =
                    serde_json::to_vec_pretty(self).context("serializing registry to JSON")?;
                std::fs::write(path, json)
            }
        }
        .with_context(|| format!("writing registry file at {}", path.display()))
    }

    /// Convenience wrapper around [`Self::save`] using [`Self::default_path`].
    pub fn save_default(&self) -> Result<()> {
        self.save(Self::default_path()?)
    }

    /// Resolves a spoken destination to a deterministic route (PRD §8
    /// steps 2-3). Matching is case-insensitive and whitespace-normalized
    /// on both the spoken input and every entry's aliases (see [`normalize`]).
    ///
    /// Returns:
    /// - [`Resolution::Single`] when **exactly one** entry has a matching
    ///   alias -- the deterministic target, no vision needed.
    /// - [`Resolution::Ambiguous`] when **more than one** entry matches.
    ///   Per PRD §8 this must become a user choice prompt, never an
    ///   autonomous guess -- so every matching entry is returned and the
    ///   caller is forced to disambiguate (there is deliberately no
    ///   "pick the first" shortcut).
    /// - [`Resolution::NotFound`] when no entry matches.
    ///
    /// "Matches" means an entry has at least one alias equal (after
    /// normalization) to the spoken destination. A single entry that lists
    /// the same alias twice, or matches on two of its own aliases, still
    /// counts as one match (dedup is by entry identity/position, not by
    /// alias), so a well-formed entry can never make *itself* ambiguous --
    /// ambiguity only arises across *distinct* entries.
    pub fn resolve<'a>(&'a self, spoken_destination: &str) -> Resolution<'a> {
        let needle = normalize(spoken_destination);
        if needle.is_empty() {
            return Resolution::NotFound;
        }

        let matches: Vec<&'a RegistryEntry> = self
            .entries
            .iter()
            .filter(|entry| entry.alias.iter().any(|a| normalize(a) == needle))
            .collect();

        match matches.len() {
            0 => Resolution::NotFound,
            1 => Resolution::Single(matches[0]),
            _ => Resolution::Ambiguous(matches),
        }
    }
}
