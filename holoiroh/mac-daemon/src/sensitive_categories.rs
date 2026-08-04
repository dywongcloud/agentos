//! Stores class-5 sensitive-app categories and their policy settings.
//!
//! Project Aro product requirements document (PRD) §9 defines six action classes numbered 0 through 5.
//! Class 5 covers sensitive targets such as password managers, banking, health, and system settings.
//! The default setting requires approval instead of blocking access.
//! Each category supports `always_ask`, `always_allow`, or `hard_block`.
//!
//! This module provides the category model, default macOS bundle identifiers, and configuration persistence.
//! The daemon loads or initializes `~/.holoiroh/sensitive_categories.toml` when it constructs `HoloControlBridge`.
//! If loading fails, the daemon logs a warning and uses built-in defaults for that run.
//! Callers can also use JavaScript Object Notation (JSON) through [`ConfigFormat`].
//! The default format is Tom's Obvious, Minimal Language (TOML).
//!
//! ## Live enforcement
//!
//! `crate::holo_bridge::control::sensitive_watchdog` polls the frontmost app once each second during a turn.
//! It obtains the bundle identifier from `crate::frontmost_app`.
//! It then calls [`SensitiveCategories::classify`] and enforces the matched category setting.
//!
//! - `always_ask` pauses the turn and sends a P0-14 `sensitive_access_consent` `input_request` over the control channel.
//! - `hard_block` cancels the turn.
//! - `always_allow` lets the turn continue.
//!
//! The app answers the consent request.
//! `examples/consent_probe.rs` exercises `always_ask` and `hard_block` against a running daemon.
//! The probe selects behavior through `CONSENT_PROBE_EXPECT` and `CONSENT_PROBE_ANSWER`.
//! The live watchdog contains an `always_allow` branch, but this probe does not select it.
//!
//! ## Classification limits
//!
//! The watchdog uses the frontmost app as a proxy for the surface that the agent will use.
//! The classifier does not inspect windows, screens, browser tabs, or Uniform Resource Locators (URLs).
//!
//! - A bundle identifier names an app, not a screen within that app.
//! - `com.apple.systempreferences` cannot distinguish Wi-Fi status from FileVault recovery keys.
//! - A browser bundle identifier cannot identify a banking site or cloud admin console in a tab.
//! - URL-level and tab-level classification are outside this module.
//! - The default lists favor the United States English market and are incomplete.
//!
//! [`SensitiveCategories::default_categories`] provides an editable seed, not an authoritative registry.
//!
//! ## Dead-code allowance
//!
//! The daemon uses configuration loading and classification during live turns.
//! Other public helpers remain available to probes and future settings interfaces.
//! The module-level allowance avoids separate `#[allow(dead_code)]` attributes on those helpers.

#![allow(dead_code)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Selects the class-5 policy for one category.
/// PRD §9 point 5 defines these settings.
///
/// Users cannot configure separate class-3 credential handling here.
/// [`CategorySetting::AlwaysAllow`] bypasses only this class-5 watchdog gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategorySetting {
    /// Requests consent unless the current turn already has an allowance for this category.
    /// This is the default.
    AlwaysAsk,
    /// Lets this watchdog continue without requesting consent.
    /// This setting cannot alter separate class-3 policy behavior.
    AlwaysAllow,
    /// Cancels the turn when this category becomes the frontmost app.
    HardBlock,
}

impl Default for CategorySetting {
    fn default() -> Self {
        CategorySetting::AlwaysAsk
    }
}

/// Defines one class-5 category and its current policy setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveCategory {
    /// Identifies the category with a stable snake_case TOML table key.
    pub id: String,
    /// User-visible category name.
    pub display_name: String,
    /// Describes the category in one line using PRD §9 terminology.
    pub description: String,
    /// Editable bundle identifiers that this category matches exactly.
    /// The list is illustrative and not exhaustive.
    pub bundle_ids: Vec<String>,
    /// Current policy setting.
    /// Serde defaults this field to [`CategorySetting::AlwaysAsk`].
    #[serde(default)]
    pub setting: CategorySetting,
}

/// Contains the configured sensitive categories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensitiveCategories {
    pub categories: Vec<SensitiveCategory>,
}

/// Selects an on-disk configuration format.
///
/// [`SensitiveCategories::default_path`] uses TOML for convenient manual editing.
/// JSON remains available for callers and tools that prefer it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Toml,
    Json,
}

impl ConfigFormat {
    /// Returns JSON for a `.json` extension, ignoring case.
    /// Returns TOML for every other extension and for no extension.
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("json") => ConfigFormat::Json,
            _ => ConfigFormat::Toml,
        }
    }
}

impl SensitiveCategories {
    /// Returns `~/.holoiroh/sensitive_categories.toml` under `$HOME`.
    ///
    /// Returns an error when `$HOME` is not set.
    pub fn default_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home)
            .join(".holoiroh")
            .join("sensitive_categories.toml"))
    }

    /// Returns `~/.holoiroh/sensitive_categories.json` under `$HOME`.
    ///
    /// Returns an error when `$HOME` is not set.
    pub fn default_json_path() -> Result<PathBuf> {
        let home = std::env::var_os("HOME")
            .context("HOME environment variable is not set (required to locate ~/.holoiroh/)")?;
        Ok(PathBuf::from(home)
            .join(".holoiroh")
            .join("sensitive_categories.json"))
    }

    /// Builds the eight PRD §9 categories in PRD order.
    /// Every category starts with [`CategorySetting::AlwaysAsk`].
    ///
    /// The bundle identifiers are illustrative vendor identifiers.
    /// We did not exhaustively audit them against `/Applications` on a live Mac.
    /// Edit the generated configuration for each deployment.
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

    /// Loads configuration in the format that [`ConfigFormat::from_path`] selects.
    ///
    /// Returns [`Self::default_categories`] when the file does not exist.
    /// This function does not save those defaults.
    /// Use [`Self::load_or_init`] to create a missing file.
    ///
    /// Returns an error for other input/output failures, invalid 8-bit Unicode Transformation Format (UTF-8), or parse failures.
    /// This behavior prevents a corrupt file from silently replacing user policy settings.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default_categories());
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("reading sensitive-categories file at {}", path.display())
                });
            }
        };

        match ConfigFormat::from_path(path) {
            ConfigFormat::Toml => {
                let text = String::from_utf8(bytes).with_context(|| {
                    format!(
                        "sensitive-categories file at {} is not valid UTF-8",
                        path.display()
                    )
                })?;
                toml::from_str(&text).with_context(|| {
                    format!("parsing sensitive-categories TOML at {}", path.display())
                })
            }
            ConfigFormat::Json => serde_json::from_slice(&bytes).with_context(|| {
                format!("parsing sensitive-categories JSON at {}", path.display())
            }),
        }
    }

    /// Loads configuration from [`Self::default_path`].
    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path()?)
    }

    /// Loads an existing file or saves [`Self::default_categories`] to a missing file.
    ///
    /// Returns the loaded or newly saved value.
    /// Returns an error when loading or initialization fails.
    pub fn load_or_init(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            return Self::load(path);
        }
        let defaults = Self::default_categories();
        defaults.save(path).with_context(|| {
            format!(
                "writing default sensitive-categories file at {}",
                path.display()
            )
        })?;
        Ok(defaults)
    }

    /// Loads or initializes `~/.holoiroh/sensitive_categories.toml`.
    pub fn load_or_init_default() -> Result<Self> {
        Self::load_or_init(Self::default_path()?)
    }

    /// Serializes the full configuration in the format that [`ConfigFormat::from_path`] selects.
    /// Creates the parent directory when necessary.
    /// Overwrites an existing file.
    ///
    /// Callers must serialize concurrent writes.
    /// The daemon currently supports one concurrent control-channel connection.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "creating sensitive-categories directory {}",
                    parent.display()
                )
            })?;
        }

        match ConfigFormat::from_path(path) {
            ConfigFormat::Toml => {
                let text = toml::to_string_pretty(self)
                    .context("serializing sensitive categories to TOML")?;
                std::fs::write(path, text)
            }
            ConfigFormat::Json => {
                let json = serde_json::to_vec_pretty(self)
                    .context("serializing sensitive categories to JSON")?;
                std::fs::write(path, json)
            }
        }
        .with_context(|| format!("writing sensitive-categories file at {}", path.display()))
    }

    /// Saves configuration to [`Self::default_path`].
    pub fn save_default(&self) -> Result<()> {
        self.save(Self::default_path()?)
    }

    /// Returns the first category whose bundle-identifier list contains `bundle_id`.
    ///
    /// Matching is exact and case-sensitive.
    /// The function does not inspect app content, windows, browser tabs, or URLs.
    /// If multiple categories contain the identifier, list order determines the result.
    pub fn classify(&self, bundle_id: &str) -> Option<&SensitiveCategory> {
        self.categories
            .iter()
            .find(|c| c.bundle_ids.iter().any(|b| b == bundle_id))
    }

    /// Returns the category with the specified stable identifier.
    pub fn find_by_id(&self, id: &str) -> Option<&SensitiveCategory> {
        self.categories.iter().find(|c| c.id == id)
    }

    /// Returns mutable access to the category with the specified stable identifier.
    /// Call [`Self::save`] to persist a changed setting.
    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut SensitiveCategory> {
        self.categories.iter_mut().find(|c| c.id == id)
    }

    /// Returns all configured bundle identifiers as a deduplicated set.
    /// This diagnostic helper can reveal cross-category duplicates in edited configuration.
    pub fn all_bundle_ids(&self) -> HashSet<&str> {
        self.categories
            .iter()
            .flat_map(|c| c.bundle_ids.iter().map(|b| b.as_str()))
            .collect()
    }
}
