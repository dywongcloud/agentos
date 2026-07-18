//! Tiny dependency-free duration parser for the `--rotate-every` CLI flag (see `main.rs`).
//!
//! Deliberately hand-rolled rather than pulling `humantime` for one parse: it accepts a single
//! unit-suffixed integer or a concatenation of them (`30m`, `2h`, `90s`, `1h30m`), with units
//! `s`/`m`/`h`/`d`. Lives in the lib crate (not just `main.rs`) so
//! `examples/ticket_rotation_probe.rs` can witness it directly against the real function, per
//! this repo's no-unit-tests rule.

use std::time::Duration;

/// Parse a `--rotate-every` duration like `30m`, `2h`, `90s`, `1h30m`. Returns an error string on
/// anything malformed (empty, non-numeric, unknown unit, missing unit, zero total, overflow).
pub fn parse_rotate_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty duration".to_string());
    }
    let mut total_secs: u64 = 0;
    let mut num = String::new();
    let mut saw_any = false;
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            num.push(ch);
            continue;
        }
        if num.is_empty() {
            return Err(format!("expected a number before unit '{ch}' in {s:?}"));
        }
        let value: u64 = num
            .parse()
            .map_err(|_| format!("number {num:?} in {s:?} is out of range"))?;
        let unit_secs = match ch {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            other => {
                return Err(format!("unknown duration unit '{other}' in {s:?} (use s/m/h/d)"));
            }
        };
        total_secs = total_secs
            .checked_add(
                value
                    .checked_mul(unit_secs)
                    .ok_or_else(|| format!("{s:?} overflows"))?,
            )
            .ok_or_else(|| format!("{s:?} overflows"))?;
        num.clear();
        saw_any = true;
    }
    if !num.is_empty() {
        return Err(format!(
            "trailing number {num:?} in {s:?} has no unit (use s/m/h/d)"
        ));
    }
    if !saw_any || total_secs == 0 {
        return Err(format!(
            "duration {s:?} must be a positive time like 30m or 2h"
        ));
    }
    Ok(Duration::from_secs(total_secs))
}
