//! Boot-time memory budget resolver — ADR 0073 §1 made executable.
//!
//! Every RedDB process resolves exactly one memory budget at boot. The
//! precedence chain is first-hit-wins:
//!
//! 1. **`config`** — explicit operator configuration: the
//!    `RedDBOptions::with_memory_budget` startup option, or the
//!    `REDDB_MEMORY_BUDGET` environment variable when the option is absent.
//! 2. **`profile-default`** — deployment-profile default. Serverless ships a
//!    strict default; embedded / primary-replica / cluster fall through to
//!    host detection.
//! 3. **`cgroup-v2`** then **`cgroup-v1`** — the container limit.
//!    A cgroup v2 `memory.max` of `max` (and the v1 "no limit" sentinel)
//!    means *unlimited*, which falls through rather than resolving.
//! 4. **`physical-fraction`** — physical RAM times a conservative fraction.
//!
//! There is **no unlimited mode**: the absence of configuration means the
//! detected default, never infinity. Every tier yields a strictly positive
//! budget, and the resolved budget is immutable for the process lifetime.
//!
//! This module deliberately resolves and reports; it neither sizes pools nor
//! enforces admission. Those are downstream slices that consume the number.

use std::sync::Once;

use super::profile::DeployProfile;

/// Environment variable carrying the explicit operator budget when no
/// startup option is supplied. Same `REDDB_*` convention as the rest of
/// the boot-time knobs (see `runtime::config_overlay`).
pub const MEMORY_BUDGET_ENV: &str = "REDDB_MEMORY_BUDGET";

/// cgroup v2 unified-hierarchy memory ceiling. Holds either a byte count
/// or the literal `max`.
pub const CGROUP_V2_MEMORY_MAX_PATH: &str = "/sys/fs/cgroup/memory.max";

/// cgroup v1 memory controller ceiling. Holds a byte count; "no limit" is
/// expressed as a huge sentinel rather than a keyword.
pub const CGROUP_V1_MEMORY_LIMIT_PATH: &str = "/sys/fs/cgroup/memory/memory.limit_in_bytes";

/// Percentage of physical RAM claimed when nothing else declares a ceiling.
///
/// Conservative on purpose: RedDB is not alone on the box. The remaining 30%
/// absorbs the kernel page cache backing our own file reads, allocator
/// fragmentation and arena slack (the budget accounts logical bytes, not RSS),
/// and whatever else shares the host. Overshooting here re-creates exactly the
/// OOM-kill class ADR 0073 exists to remove, and the operator cannot observe
/// the mistake until the kernel makes it for them.
pub const PHYSICAL_RAM_BUDGET_PERCENT: u64 = 70;

/// Strict default for the serverless profile, where boundedness is a survival
/// contract rather than a tuning preference (ADR 0038 §1). Sized to fit the
/// smallest slot classes offered by mainstream function runtimes so the same
/// binary is safe there without configuration.
pub const SERVERLESS_PROFILE_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// Floor applied to the `physical-fraction` tier only.
///
/// Physical-RAM detection returns 0 when `/proc/meminfo` is unreadable or the
/// platform is unsupported; a 0-byte budget would violate the "always > 0"
/// invariant and make every downstream pool degenerate. Operator-declared and
/// cgroup-derived budgets are *facts* and are never raised to this floor —
/// only the guess is.
pub const MINIMUM_DETECTED_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// cgroup v1 writes `PAGE_COUNTER_MAX` (`i64::MAX` rounded down to a page
/// boundary) into `memory.limit_in_bytes` when the controller is unlimited.
/// Kernels differ in the exact page size used, so treat anything at or above
/// this as "no limit" rather than comparing for equality.
const CGROUP_V1_UNLIMITED_SENTINEL: u64 = 0x7FFF_FFFF_FFFF_F000;

/// Which tier of the precedence chain produced the resolved budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryBudgetSource {
    /// Explicit operator configuration (startup option or `REDDB_MEMORY_BUDGET`).
    Config,
    /// Deployment-profile default.
    ProfileDefault,
    /// cgroup v2 `memory.max`.
    CgroupV2,
    /// cgroup v1 `memory.limit_in_bytes`.
    CgroupV1,
    /// Physical RAM times `PHYSICAL_RAM_BUDGET_PERCENT`.
    PhysicalFraction,
}

impl MemoryBudgetSource {
    /// Stable label echoed by the boot log and the `red.stats` budget section.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::ProfileDefault => "profile-default",
            Self::CgroupV2 => "cgroup-v2",
            Self::CgroupV1 => "cgroup-v1",
            Self::PhysicalFraction => "physical-fraction",
        }
    }
}

/// The one budget a process runs under. Immutable for the process lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudget {
    pub resolved_bytes: u64,
    pub source: MemoryBudgetSource,
}

/// A configured budget that is not a positive byte count. Never silently
/// replaced by a default — boot fails so the operator sees their typo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidMemoryBudget {
    pub raw: String,
}

impl std::fmt::Display for InvalidMemoryBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid memory budget `{}`: expected a positive byte count with an optional \
             unit suffix (`536870912`, `512MiB`, `2GiB`, `2GB`); RedDB has no unlimited \
             mode — omit the setting to use the detected default",
            self.raw
        )
    }
}

impl std::error::Error for InvalidMemoryBudget {}

/// Everything the resolver reads, captured as plain data so the cgroup and
/// physical-RAM probes are injectable from tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryBudgetInputs {
    /// Raw operator-configured value, exactly as written. `None` when unset.
    pub configured: Option<String>,
    /// Deployment profile of this process.
    pub profile: Option<DeployProfile>,
    /// Raw contents of cgroup v2 `memory.max`, if the file exists.
    pub cgroup_v2_memory_max: Option<String>,
    /// Raw contents of cgroup v1 `memory.limit_in_bytes`, if the file exists.
    pub cgroup_v1_memory_limit: Option<String>,
    /// Physical RAM in bytes; 0 when undetectable.
    pub physical_ram_bytes: u64,
}

/// Walk the precedence chain and return the one budget for this process.
///
/// The only failure mode is a nonsensical *configured* value. Detection tiers
/// cannot fail: an unreadable or unlimited cgroup falls through, and physical
/// detection bottoms out at `MINIMUM_DETECTED_BUDGET_BYTES`.
pub fn resolve(inputs: &MemoryBudgetInputs) -> Result<MemoryBudget, InvalidMemoryBudget> {
    if let Some(raw) = inputs.configured.as_deref() {
        let resolved_bytes = parse_budget_bytes(raw).ok_or_else(|| InvalidMemoryBudget {
            raw: raw.to_string(),
        })?;
        return Ok(MemoryBudget {
            resolved_bytes,
            source: MemoryBudgetSource::Config,
        });
    }

    if let Some(resolved_bytes) = inputs.profile.and_then(profile_default_bytes) {
        return Ok(MemoryBudget {
            resolved_bytes,
            source: MemoryBudgetSource::ProfileDefault,
        });
    }

    if let Some(resolved_bytes) = inputs
        .cgroup_v2_memory_max
        .as_deref()
        .and_then(parse_cgroup_v2_limit)
    {
        return Ok(MemoryBudget {
            resolved_bytes,
            source: MemoryBudgetSource::CgroupV2,
        });
    }

    if let Some(resolved_bytes) = inputs
        .cgroup_v1_memory_limit
        .as_deref()
        .and_then(parse_cgroup_v1_limit)
    {
        return Ok(MemoryBudget {
            resolved_bytes,
            source: MemoryBudgetSource::CgroupV1,
        });
    }

    Ok(MemoryBudget {
        resolved_bytes: physical_fraction_bytes(inputs.physical_ram_bytes),
        source: MemoryBudgetSource::PhysicalFraction,
    })
}

/// Probe the live host: the startup option (or `REDDB_MEMORY_BUDGET`), the
/// deployment profile, both cgroup files, and physical RAM.
pub fn host_inputs(profile: DeployProfile, configured_bytes: Option<u64>) -> MemoryBudgetInputs {
    // The startup option and the env var share one validation path, so a
    // `Some(0)` option is rejected exactly like `REDDB_MEMORY_BUDGET=0`.
    let configured = configured_bytes
        .map(|bytes| bytes.to_string())
        .or_else(|| std::env::var(MEMORY_BUDGET_ENV).ok())
        .filter(|raw| !raw.trim().is_empty());

    MemoryBudgetInputs {
        configured,
        profile: Some(profile),
        cgroup_v2_memory_max: read_cgroup_file(CGROUP_V2_MEMORY_MAX_PATH),
        cgroup_v1_memory_limit: read_cgroup_file(CGROUP_V1_MEMORY_LIMIT_PATH),
        physical_ram_bytes: physical_ram_bytes(),
    }
}

/// Resolve the process budget from the live host. Called once per process at
/// boot; a nonsensical configured value fails the boot didactically.
pub fn resolve_for_boot(
    profile: DeployProfile,
    configured_bytes: Option<u64>,
) -> Result<MemoryBudget, InvalidMemoryBudget> {
    resolve(&host_inputs(profile, configured_bytes))
}

/// Emit the boot log line stating the resolved budget and its source tier.
/// Guarded so a process that opens several runtimes still logs exactly once.
pub fn log_resolved_once(budget: &MemoryBudget) {
    static LOGGED: Once = Once::new();
    LOGGED.call_once(|| {
        tracing::info!(
            resolved_bytes = budget.resolved_bytes,
            source = budget.source.as_str(),
            "memory budget resolved"
        );
    });
}

/// Strict per-profile defaults. `None` means "fall through to host detection".
fn profile_default_bytes(profile: DeployProfile) -> Option<u64> {
    match profile {
        DeployProfile::Serverless => Some(SERVERLESS_PROFILE_BUDGET_BYTES),
        DeployProfile::Embedded | DeployProfile::PrimaryReplica | DeployProfile::Cluster => None,
    }
}

/// Parse an operator-supplied budget: a positive integer with an optional
/// unit suffix. Returns `None` for every other shape — including `0`, a
/// leading `-`, and words like `unlimited`.
fn parse_budget_bytes(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    let digits_end = trimmed
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map_or(trimmed.len(), |(idx, _)| idx);
    if digits_end == 0 {
        return None;
    }

    let value = trimmed[..digits_end].parse::<u64>().ok()?;
    let multiplier = match trimmed[digits_end..].trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "tb" => 1_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        _ => return None,
    };

    let bytes = value.checked_mul(multiplier)?;
    (bytes > 0).then_some(bytes)
}

/// cgroup v2: `max` (or anything unparseable) means no limit — fall through.
fn parse_cgroup_v2_limit(raw: &str) -> Option<u64> {
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("max") {
        return None;
    }
    trimmed.parse::<u64>().ok().filter(|bytes| *bytes > 0)
}

/// cgroup v1: the sentinel (or anything unparseable) means no limit.
fn parse_cgroup_v1_limit(raw: &str) -> Option<u64> {
    raw.trim()
        .parse::<u64>()
        .ok()
        .filter(|bytes| *bytes > 0 && *bytes < CGROUP_V1_UNLIMITED_SENTINEL)
}

/// The last tier. Never returns 0 — see `MINIMUM_DETECTED_BUDGET_BYTES`.
fn physical_fraction_bytes(physical_ram_bytes: u64) -> u64 {
    // Divide before multiplying: `physical * 70` overflows only on absurd
    // inputs, but the rounding loss here is at most 99 bytes.
    (physical_ram_bytes / 100)
        .saturating_mul(PHYSICAL_RAM_BUDGET_PERCENT)
        .max(MINIMUM_DETECTED_BUDGET_BYTES)
}

/// A missing cgroup file is the common case (no container, or the other
/// hierarchy version). Unreadable is treated the same as missing.
fn read_cgroup_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(target_os = "linux")]
fn physical_ram_bytes() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| {
            meminfo
                .lines()
                .find(|line| line.starts_with("MemTotal:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .map_or(0, |kb| kb.saturating_mul(1024))
}

#[cfg(not(target_os = "linux"))]
fn physical_ram_bytes() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn inputs() -> MemoryBudgetInputs {
        MemoryBudgetInputs {
            profile: Some(DeployProfile::Embedded),
            physical_ram_bytes: 16 * GIB,
            ..MemoryBudgetInputs::default()
        }
    }

    #[test]
    fn explicit_configuration_wins_over_every_other_tier() {
        let resolved = resolve(&MemoryBudgetInputs {
            configured: Some("512MiB".to_string()),
            profile: Some(DeployProfile::Serverless),
            cgroup_v2_memory_max: Some("1073741824".to_string()),
            cgroup_v1_memory_limit: Some("2147483648".to_string()),
            ..inputs()
        })
        .expect("configured budget resolves");

        assert_eq!(resolved.source, MemoryBudgetSource::Config);
        assert_eq!(resolved.resolved_bytes, 512 * 1024 * 1024);
    }

    #[test]
    fn configured_budget_accepts_bare_bytes_and_unit_suffixes() {
        let cases = [
            ("536870912", 536_870_912),
            (" 536870912 ", 536_870_912),
            ("1024b", 1024),
            ("2KiB", 2048),
            ("512MiB", 512 * 1024 * 1024),
            ("2GiB", 2 * GIB),
            ("1TiB", 1 << 40),
            ("2GB", 2_000_000_000),
            ("500mb", 500_000_000),
        ];

        for (raw, expected) in cases {
            assert_eq!(parse_budget_bytes(raw), Some(expected), "parsing {raw}");
        }
    }

    #[test]
    fn nonsensical_configured_budget_is_rejected_not_replaced() {
        for raw in [
            "0",
            "0MiB",
            "-1",
            "unlimited",
            "max",
            "",
            "  ",
            "12 apples",
            "MiB",
            "1.5GiB",
        ] {
            let err = resolve(&MemoryBudgetInputs {
                configured: Some(raw.to_string()),
                ..inputs()
            })
            .expect_err("nonsensical configured budget must fail boot");

            let message = err.to_string();
            assert!(
                message.contains("expected a positive byte count"),
                "{message}"
            );
            assert!(
                message.contains("512MiB"),
                "error names the valid form: {message}"
            );
            assert!(message.contains("no unlimited mode"), "{message}");
        }
    }

    #[test]
    fn serverless_profile_ships_a_strict_default() {
        let resolved = resolve(&MemoryBudgetInputs {
            profile: Some(DeployProfile::Serverless),
            cgroup_v2_memory_max: Some("34359738368".to_string()),
            ..inputs()
        })
        .expect("profile default resolves");

        assert_eq!(resolved.source, MemoryBudgetSource::ProfileDefault);
        assert_eq!(resolved.resolved_bytes, SERVERLESS_PROFILE_BUDGET_BYTES);
    }

    #[test]
    fn detection_profiles_fall_through_to_the_cgroup() {
        for profile in [
            DeployProfile::Embedded,
            DeployProfile::PrimaryReplica,
            DeployProfile::Cluster,
        ] {
            let resolved = resolve(&MemoryBudgetInputs {
                profile: Some(profile),
                cgroup_v2_memory_max: Some("1073741824\n".to_string()),
                ..inputs()
            })
            .expect("cgroup budget resolves");

            assert_eq!(resolved.source, MemoryBudgetSource::CgroupV2);
            assert_eq!(resolved.resolved_bytes, GIB);
        }
    }

    #[test]
    fn cgroup_v2_max_falls_through_to_v1_then_physical() {
        let to_v1 = resolve(&MemoryBudgetInputs {
            cgroup_v2_memory_max: Some("max\n".to_string()),
            cgroup_v1_memory_limit: Some("2147483648".to_string()),
            ..inputs()
        })
        .expect("v1 fallback resolves");
        assert_eq!(to_v1.source, MemoryBudgetSource::CgroupV1);
        assert_eq!(to_v1.resolved_bytes, 2 * GIB);

        let to_physical = resolve(&MemoryBudgetInputs {
            cgroup_v2_memory_max: Some("MAX".to_string()),
            ..inputs()
        })
        .expect("physical fallback resolves");
        assert_eq!(to_physical.source, MemoryBudgetSource::PhysicalFraction);
    }

    #[test]
    fn cgroup_v1_unlimited_sentinel_falls_through_to_physical() {
        let resolved = resolve(&MemoryBudgetInputs {
            cgroup_v1_memory_limit: Some(CGROUP_V1_UNLIMITED_SENTINEL.to_string()),
            ..inputs()
        })
        .expect("sentinel falls through");

        assert_eq!(resolved.source, MemoryBudgetSource::PhysicalFraction);
        assert_eq!(resolved.resolved_bytes, 16 * GIB / 100 * 70);
    }

    #[test]
    fn physical_fraction_is_the_last_tier_and_never_zero() {
        let detected = resolve(&inputs()).expect("physical fraction resolves");
        assert_eq!(detected.source, MemoryBudgetSource::PhysicalFraction);
        assert_eq!(detected.resolved_bytes, 16 * GIB / 100 * 70);

        // Undetectable physical RAM bottoms out at the floor rather than 0.
        let undetectable = resolve(&MemoryBudgetInputs {
            physical_ram_bytes: 0,
            ..inputs()
        })
        .expect("undetectable physical RAM resolves");
        assert_eq!(undetectable.source, MemoryBudgetSource::PhysicalFraction);
        assert_eq!(undetectable.resolved_bytes, MINIMUM_DETECTED_BUDGET_BYTES);
    }

    #[test]
    fn every_resolved_budget_is_strictly_positive() {
        let profiles = [
            None,
            Some(DeployProfile::Embedded),
            Some(DeployProfile::Serverless),
            Some(DeployProfile::PrimaryReplica),
            Some(DeployProfile::Cluster),
        ];
        let cgroup_v2 = [None, Some("max"), Some("0"), Some("1"), Some("garbage")];
        let cgroup_v1 = [None, Some("0"), Some("1"), Some("9223372036854771712")];
        let physical = [0, 1, 1024, 16 * GIB, u64::MAX];
        let configured = [None, Some("1"), Some("1TiB")];

        for profile in profiles {
            for v2 in cgroup_v2 {
                for v1 in cgroup_v1 {
                    for ram in physical {
                        for config in configured {
                            let resolved = resolve(&MemoryBudgetInputs {
                                configured: config.map(str::to_string),
                                profile,
                                cgroup_v2_memory_max: v2.map(str::to_string),
                                cgroup_v1_memory_limit: v1.map(str::to_string),
                                physical_ram_bytes: ram,
                            })
                            .expect("valid inputs always resolve");
                            assert!(resolved.resolved_bytes > 0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn source_labels_are_the_documented_tier_names() {
        assert_eq!(MemoryBudgetSource::Config.as_str(), "config");
        assert_eq!(
            MemoryBudgetSource::ProfileDefault.as_str(),
            "profile-default"
        );
        assert_eq!(MemoryBudgetSource::CgroupV2.as_str(), "cgroup-v2");
        assert_eq!(MemoryBudgetSource::CgroupV1.as_str(), "cgroup-v1");
        assert_eq!(
            MemoryBudgetSource::PhysicalFraction.as_str(),
            "physical-fraction"
        );
    }
}
