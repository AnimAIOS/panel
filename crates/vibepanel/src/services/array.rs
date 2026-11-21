//! Filesystem-independent representation of a multi-device array.
//!
//! Filesystem-specific parsers (`bcachefs`, `btrfs`) produce their own details
//! and convert them into [`ArrayInfo`], which is what the snapshot carries and
//! the popover renders. Keeping this layer here avoids duplicating the per-device
//! rows for every filesystem we support.

/// Which command produced the array details, used only for the display name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayKind {
    Bcachefs,
    Btrfs,
}

impl ArrayKind {
    /// Name shown in the overview line; deliberately not localized.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bcachefs => "bcachefs",
            Self::Btrfs => "btrfs",
        }
    }
}

/// One device of a multi-device filesystem, in a form the popover can render.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArrayDevice {
    /// Short source label, e.g. `ultra_fast (device 2)` or `devid 1`.
    pub label: String,
    /// Block device path or name, e.g. `nvme0n1p2` or `/dev/sdc1`.
    pub device: String,
    /// State as reported by the filesystem. Anything other than `rw` is
    /// highlighted as an error.
    pub state: String,
    pub size: u64,
    pub used: u64,
    /// Percentage of the device in use; absent when it cannot be determined.
    pub use_percent: Option<u32>,
}

/// Array details carried in a [`super::system::MountUsage`] snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayInfo {
    pub devices: Vec<ArrayDevice>,
    /// Redundancy level, e.g. `2x` or `RAID1`; `None` means single/no redundancy.
    pub redundancy: Option<String>,
    pub degraded: bool,
    pub kind: ArrayKind,
}

/// Parse a raw byte count or a human-readable size (`3.84T`, `2.73TiB`, `50%` is
/// not accepted). Shared by the filesystem parsers so degradation and device
/// sizes work with both raw and human-readable command output.
pub(crate) fn parse_size(value: &str) -> Option<u64> {
    if let Ok(bytes) = value.parse::<u64>() {
        return Some(bytes);
    }
    let split = value
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split);
    let multiplier = match suffix.to_ascii_uppercase().as_str() {
        "" => 1,
        "K" | "KIB" => 1_024,
        "M" | "MIB" => 1_024u64.pow(2),
        "G" | "GIB" => 1_024u64.pow(3),
        "T" | "TIB" => 1_024u64.pow(4),
        "P" | "PIB" => 1_024u64.pow(5),
        "E" | "EIB" => 1_024u64.pow(6),
        _ => return None,
    };
    let number: f64 = number.parse().ok()?;
    Some((number * multiplier as f64) as u64)
}
