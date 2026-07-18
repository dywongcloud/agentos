//! Class-5 sensitive-app category data model and config file I/O.
//!
//! Project Aro PRD §9 ("Safety, Consent, and Sensitive Apps") defines a
//! five-class action policy; class 5 ("sensitive target") covers apps like
//! password managers, banking, health, and system settings, where the
//! default behavior is **approval-gated, not blocked**: the executor is
//! supposed to pause before any input into a sensitive surface, show the
//! user a sensitive-access request, and respect a per-category setting of
//! always-ask (default) / always-allow / hard-block.
//!
//! This module implements the **data model and config-file persistence**
//! for that per-category setting: the default category list (with
//! illustrative macOS bundle IDs), the three-way setting enum, and
//! load/save against a user-editable file at `~/.holoiroh/sensitive_categories.toml`
//! (or `.json` -- see [`ConfigFormat`]).
//!
//! ## What this module is *not*
//!
//! This is a config-file row, not a policy-enforcement row. Nothing in this
//! codebase currently calls into this module from a live interception
//! point -- there is no `ComputerUseExecutor`/policy-wrapper equivalent
//! built yet in this Rust daemon (`holo_bridge` forwards prompts straight
//! through to `holo serve`; see that module's own docs). Wiring "pause
//! before any input into a sensitive surface" into a real interception
//! point requires:
//!
//! 1. A foregrounded-app / target-app classifier (this module only offers
//!    a **bundle-ID lookup**, see [`SensitiveCategories::classify`]'s doc
//!    for exactly how heuristic that is).
//! 2. The not-yet-built approval-request round trip to the iPhone app (the
//!    PRD's `sensitive_access_requested` interactive-waiting state, P0-14
//!    input-request payloads, manual_input/pairing UI) -- none of that
//!    exists in this repo yet (see `holoiroh/README.md`'s "Status" section:
//!    the iOS app is a SwiftUI skeleton with no real transport).
//! 3. A hook inside whatever eventually plays the role of "the executor"
//!    in this Rust codebase, to actually consult [`SensitiveCategories`]
//!    before an action runs and to actually pause/reject/allow based on
//!    the result.
//!
//! None of that exists after this change. This module only makes it
//! possible to *ask* "is `com.1password.1password` in a sensitive category,
//! and what's the configured setting for that category" and to persist a
//! user's edits to that configuration -- it does not make anything ask.
//!
//! ## Why bundle-ID matching is a heuristic, not a classifier
//!
//! There is no real app-classification pipeline in this alpha. The default
//! lists below are a best-effort, illustrative starting point (common macOS
//! apps for each PRD-listed category), not an exhaustive or authoritative
//! registry:
//!
//! - A bundle ID identifies *an application*, not a *screen inside it* --
//!   e.g. matching `com.apple.systempreferences` catches System Settings
//!   entirely, but can't distinguish "the user is looking at Wi-Fi status"
//!   from "the user is looking at FileVault recovery keys". PRD §9's class
//!   5 is defined at the level of "sensitive target" surfaces, which in
//!   general is finer-grained than one bundle ID.
//! - Browser-based instances of these categories (a banking website open
//!   in Safari/Chrome, a cloud admin console in a browser tab) are entirely
//!   unaddressed -- the browser's bundle ID (e.g. `com.apple.Safari`) gives
//!   no visibility into which site/tab is active. A real implementation
//!   would need URL/tab-level classification, which is out of scope here.
//! - The lists are US/English-market-biased and inevitably incomplete --
//!   this is exactly why the config file is user-editable rather than a
//!   hardcoded constant with no override path.
//!
//! Treat [`SensitiveCategories::default_categories`] as a seed a real
//! deployment is expected to edit, not a finished registry.
//!
//! ## Why `#![allow(dead_code)]`
//!
//! Nothing in `main.rs` calls into this module yet (see "What this module
//! is not" above) -- this pass only adds the module and registers it via
//! `mod sensitive_categories;` so it's compiled and reachable for a future
//! policy-interception row to call, and so `examples/sensitive_categories_probe.rs`
//! can exercise it for real. Every item here is real, working, documented
//! public API (same status `allowlist.rs`'s own not-yet-called methods
//! carry, each with its own `#[allow(dead_code)]`) -- this blanket module-
//! level attribute just avoids repeating that same annotation on every
//! single method below, since *none* of them have a call site in the
//! binary yet.

#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Per-category policy setting, per PRD §9 point 5 ("Per-category settings:
/// always-ask (default), always-allow, or hard-block -- the user's
/// configuration wins").
///
/// Note what this enum does *not* cover: PRD §9 point 4 says "credential
/// surfaces inside the app still pause per class 3 regardless of approval
/// -- the credential boundary is not user-configurable". That class-3
/// credential pause is a separate, non-configurable behavior this enum has
/// no variant for and must never be used to bypass -- even a category set
/// to [`CategorySetting::AlwaysAllow`] does not touch class-3 credential
/// handling, because nothing in this module is wired to class 3 at all
/// (see the module doc's "What this module is not").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategorySetting {
    /// Default. Every class-5 entry into this category should trigger the
    /// approval-request round trip (not yet built -- see module doc).
    AlwaysAsk,
    /// User has pre-approved this category; a real policy layer would skip
    /// the interactive approval round trip (still subject to the
    /// non-configurable class-3 credential pause, which this setting
    /// cannot affect).
    AlwaysAllow,
    /// User has pre-rejected this category; a real policy layer would
    /// refuse entry outright rather than asking.
    HardBlock,
}

impl Default for CategorySetting {
    fn default() -> Self {
        CategorySetting::AlwaysAsk
    }
}

/// One class-5 sensitive category: a human-readable name, the PRD-quoted
/// description of what it covers, a best-effort list of known macOS bundle
/// IDs (see module doc for how heuristic this is), and the per-category
/// [`CategorySetting`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveCategory {
    /// Short machine-stable identifier (snake_case), used for lookups and
    /// as the TOML table key on disk. Not shown to the end user directly --
    /// [`Self::display_name`] is for that.
    pub id: String,
    /// Human-readable name shown to the user (e.g. in a future approval
    /// prompt or a settings UI).
    pub display_name: String,
    /// One-line description of what belongs in this category, matching
    /// PRD §9's own category list wording where applicable.
    pub description: String,
    /// Best-effort, illustrative macOS bundle IDs known to fall in this
    /// category. Not exhaustive -- see module doc's "Why bundle-ID
    /// matching is a heuristic" section. User-editable via the config
    /// file; additions/removals here are exactly the PRD's "user-
    /// configured additions/removals" for this category.
    pub bundle_ids: Vec<String>,
    /// This category's current policy setting. Defaults to
    /// [`CategorySetting::AlwaysAsk`] per PRD §9 point 5.
    #[serde(default)]
    pub setting: CategorySetting,
}

/// The full set of sensitive categories, as loaded from (or defaulted for)
/// the user-editable config file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveCategories {
    pub categories: Vec<SensitiveCategory>,
}

/// Which on-disk format to read/write. Both are supported per the task's
/// ask ("TOML/JSON config file"); TOML is the default (see
/// [`SensitiveCategories::default_path`]) because it's the friendlier
/// format for a human to hand-edit (comments, less punctuation), but JSON
/// is offered as an equally-real alternative for callers/tooling that
/// prefer it (e.g. something that already speaks JSON elsewhere in this
/// crate, like `allowlist.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Toml,
    Json,
}

impl ConfigFormat {
    /// Infers the format from a path's extension. Defaults to
    /// [`ConfigFormat::Toml`] for any extension other than a recognized
    /// `.json` (including no extension at all), matching this module's
    /// TOML-first default path.
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("json") => ConfigFormat::Json,
            _ => ConfigFormat::Toml,
        }
    }
}

impl SensitiveCategories {
    /// Default config file location: `~/.holoiroh/sensitive_categories.toml`.
    /// Resolved via `$HOME` (same approach as [`crate::allowlist::Allowlist::default_path`]
    /// -- this daemon is macOS-only, where `$HOME` is always set for an
    /// interactive login session).
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home)
            .join(".holoiroh")
            .join("sensitive_categories.toml"))
    }

    /// The alternate JSON config file location, for callers that prefer
    /// `.json` over the default `.toml` (see [`ConfigFormat`]):
    /// `~/.holoiroh/sensitive_categories.json`.
    pub fn default_json_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home)
            .join(".holoiroh")
            .join("sensitive_categories.json"))
    }

    /// The built-in default category list: password managers, banking/
    /// brokerage, payroll/tax/legal, health, system/security settings,
    /// identity/admin consoles, device management, production
    /// infrastructure/admin dashboards -- exactly the PRD §9 list, in the
    /// order given there. Every category starts at
    /// [`CategorySetting::AlwaysAsk`], the PRD-specified default.
    ///
    /// Bundle IDs are best-effort/illustrative (see module doc) -- verified
    /// against each vendor's own publicly documented bundle identifier
    /// where the vendor publishes one, not exhaustively audited against a
    /// live Mac's `/Applications`. Treat this as a starting seed, not a
    /// finished registry: a real deployment is expected to edit the config
    /// file this seeds.
    pub fn default_categories() -> Self {
        let cat = |id: &str, display_name: &str, description: &str, bundle_ids: &[&str]| {
            SensitiveCategory {
                id: id.to_string(),
                display_name: display_name.to_string(),
                description: description.to_string(),
                bundle_ids: bundle_ids.iter().map(|s| s.to_string()).collect(),
                setting: CategorySetting::default(),
            }
        };

        SensitiveCategories {
            categories: vec![
                cat(
                    "password_managers",
                    "Password Managers",
                    "Password and secrets managers",
                    &[
                        "com.1password.1password",
                        "com.1password.1password7",
                        "com.agilebits.onepassword7",
                        "com.lastpass.LastPass",
                        "com.bitwarden.desktop",
                        "com.dashlane.dashlanephonefinal",
                        "com.apple.Passwords",
                        "com.apple.keychainaccess",
                    ],
                ),
                cat(
                    "banking_brokerage",
                    "Banking and Brokerage",
                    "Banking, brokerage, and other financial-account apps",
                    &[
                        "com.chase.sig.Chase",
                        "com.bankofamerica.BankAmericaMobile",
                        "com.wellsfargo.mobile",
                        "com.schwab.mobile",
                        "com.fidelity.stockplan",
                        "com.robinhood.release.Robinhood",
                        "com.coinbase.Coinbase",
                        "com.paypal.PPClient",
                        "com.venmo.Venmo",
                        "com.intuit.mint",
                    ],
                ),
                cat(
                    "payroll_tax_legal",
                    "Payroll, Tax, and Legal",
                    "Payroll, tax filing, and legal-document apps",
                    &[
                        "com.intuit.turbotax",
                        "com.intuit.QuickBooksDesktop",
                        "com.gusto.Gusto",
                        "com.adp.mobile",
                        "com.docusign.DocuSign",
                    ],
                ),
                cat(
                    "health",
                    "Health",
                    "Health, medical records, and telehealth apps",
                    &[
                        "com.apple.HealthApp",
                        "com.epic.mychart",
                        "com.teladoc.member",
                        "com.onemedical.onemedical",
                        "com.cvs.CVSWithSpecWeeklyAdsMigrator",
                    ],
                ),
                cat(
                    "system_security_settings",
                    "System and Security Settings",
                    "macOS System Settings and security/privacy configuration",
                    &[
                        "com.apple.systempreferences",
                        "com.apple.preference.security",
                        "com.apple.SecurityAgent",
                        "com.apple.Terminal",
                        "com.apple.ActivityMonitor",
                    ],
                ),
                cat(
                    "identity_admin_consoles",
                    "Identity and Admin Consoles",
                    "Identity providers and organization admin consoles",
                    &[
                        "com.okta.mobile",
                        "com.google.GoogleAdmin",
                        "com.duosecurity.DuoMobile",
                        "com.apple.AppleIDAuthAgent",
                    ],
                ),
                cat(
                    "device_management",
                    "Device Management",
                    "MDM enrollment and device-management apps",
                    &[
                        "com.jamf.management.jamfAAD",
                        "com.jamfsoftware.selfservice.mac",
                        "com.kandji.Kandji",
                        "com.microsoft.CompanyPortalMac",
                        "com.apple.mobiledeviceupdater",
                    ],
                ),
                cat(
                    "production_infra",
                    "Production Infrastructure",
                    "Production infrastructure and admin dashboards",
                    &[
                        "com.amazon.aws.console",
                        "com.google.Chrome",
                        "com.tinyapp.TablePlus",
                        "com.sequel-ace.sequel-ace",
                        "com.datadoghq.desktop",
                        "com.pagerduty.desktop",
                    ],
                ),
            ],
        }
    }

    /// Loads the config from `path`, in the format inferred by
    /// [`ConfigFormat::from_path`]. A missing file is **not** an error --
    /// it's the expected state on first run, and this returns
    /// [`Self::default_categories`] in that case (the caller is
    /// responsible for calling [`Self::save`] afterwards if it wants that
    /// default actually persisted to disk; [`Self::load_or_init`] does
    /// that for you). Any other I/O error, or a parse failure, is a real
    /// error -- a corrupt config silently treated as "just use defaults"
    /// would silently discard a user's hard-block/allow customizations,
    /// which is exactly the kind of silent policy change this file exists
    /// to prevent.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default_categories());
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("reading sensitive-categories file at {}", path.display()));
            }
        };

        match ConfigFormat::from_path(path) {
            ConfigFormat::Toml => {
                let text = String::from_utf8(bytes).with_context(|| {
                    format!("sensitive-categories file at {} is not valid UTF-8", path.display())
                })?;
                toml::from_str(&text)
                    .with_context(|| format!("parsing sensitive-categories TOML at {}", path.display()))
            }
            ConfigFormat::Json => serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing sensitive-categories JSON at {}", path.display())),
        }
    }

    /// Convenience wrapper around [`Self::load`] using [`Self::default_path`].
    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path()?)
    }

    /// Loads from `path` if it exists; otherwise builds
    /// [`Self::default_categories`] **and persists it** to `path` so the
    /// file exists (with sensible defaults, per the task's ask) after the
    /// first run, ready for the user to hand-edit. Returns the loaded or
    /// newly-defaulted-and-saved value either way.
    pub fn load_or_init(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            return Self::load(path);
        }
        let defaults = Self::default_categories();
        defaults
            .save(path)
            .with_context(|| format!("writing default sensitive-categories file at {}", path.display()))?;
        Ok(defaults)
    }

    /// Convenience wrapper around [`Self::load_or_init`] using
    /// [`Self::default_path`] (i.e. `~/.holoiroh/sensitive_categories.toml`).
    pub fn load_or_init_default() -> Result<Self> {
        Self::load_or_init(Self::default_path()?)
    }

    /// Writes `self` to `path`, in the format inferred by
    /// [`ConfigFormat::from_path`], creating the parent directory
    /// (`~/.holoiroh/`) if it doesn't exist yet. Overwrites the whole file,
    /// same one-writer-at-a-time posture as
    /// [`crate::allowlist::Allowlist::save`] (this daemon supports exactly
    /// one concurrent control-channel connection today, so concurrent
    /// writers to this file are not a real scenario yet either).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating sensitive-categories directory {}", parent.display()))?;
        }

        match ConfigFormat::from_path(path) {
            ConfigFormat::Toml => {
                let text = toml::to_string_pretty(self).context("serializing sensitive categories to TOML")?;
                std::fs::write(path, text)
            }
            ConfigFormat::Json => {
                let json =
                    serde_json::to_vec_pretty(self).context("serializing sensitive categories to JSON")?;
                std::fs::write(path, json)
            }
        }
        .with_context(|| format!("writing sensitive-categories file at {}", path.display()))
    }

    /// Convenience wrapper around [`Self::save`] using [`Self::default_path`].
    pub fn save_default(&self) -> Result<()> {
        self.save(Self::default_path()?)
    }

    /// Looks up which category (if any) a macOS bundle ID falls into,
    /// returning the matching [`SensitiveCategory`] by reference.
    ///
    /// This is a **bundle-ID membership check, not an app classifier** --
    /// see the module doc's "Why bundle-ID matching is a heuristic"
    /// section for exactly what this can and can't distinguish. Matching
    /// is case-sensitive and exact (macOS bundle IDs are conventionally
    /// lowercase reverse-DNS and this does not attempt fuzzy matching,
    /// which would risk false positives on an unrelated app that happens
    /// to share a prefix).
    ///
    /// Returns the first matching category if (implausibly, given the
    /// disjoint default lists) a bundle ID were listed in more than one
    /// user-edited category -- this is a config authoring choice this
    /// function does not attempt to prevent or resolve; a real policy
    /// layer consuming this would need its own conflict-resolution
    /// decision (e.g. "most restrictive wins") if that mattered, which is
    /// out of scope for the data-model row this module implements.
    pub fn classify(&self, bundle_id: &str) -> Option<&SensitiveCategory> {
        self.categories
            .iter()
            .find(|c| c.bundle_ids.iter().any(|b| b == bundle_id))
    }

    /// Looks up a category by its stable [`SensitiveCategory::id`].
    pub fn find_by_id(&self, id: &str) -> Option<&SensitiveCategory> {
        self.categories.iter().find(|c| c.id == id)
    }

    /// Mutable version of [`Self::find_by_id`], for updating a category's
    /// [`CategorySetting`] (e.g. from a future settings UI) before calling
    /// [`Self::save`].
    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut SensitiveCategory> {
        self.categories.iter_mut().find(|c| c.id == id)
    }

    /// All bundle IDs across all categories, deduplicated. Diagnostic
    /// convenience -- not currently called from any live policy path (see
    /// module doc), useful for a future `--list-sensitive-bundle-ids`
    /// command or for sanity-checking a hand-edited config file for
    /// accidental cross-category duplicates.
    pub fn all_bundle_ids(&self) -> HashSet<&str> {
        self.categories
            .iter()
            .flat_map(|c| c.bundle_ids.iter().map(|b| b.as_str()))
            .collect()
    }
}
