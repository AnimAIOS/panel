//! Parsing of `bcachefs fs usage` output for multi-device filesystems.
//!
//! The command reports both filesystem-wide replication information and a
//! per-device table. Only the parts the panel displays are kept here; running
//! the command and deciding when to refresh it belongs to `SystemService`.

use super::array::{ArrayDevice, ArrayInfo, ArrayKind, parse_size};

/// One device in a bcachefs filesystem.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BcachefsDevice {
    /// Label from the `Device label` column, e.g. `ultra_fast (device 2)`.
    pub label: String,
    /// Block device name, e.g. `nvme0n1p2`.
    pub device: String,
    /// Device state as reported by bcachefs (`rw`, `ro`, `offline`, ...).
    pub state: String,
    pub size: u64,
    pub used: u64,
    /// Column `Use%`; absent when bcachefs cannot report it.
    pub use_percent: Option<u32>,
    /// Bytes queued to be moved off the device during evacuation, if any.
    pub leaving: Option<u64>,
}

/// Array details from one `bcachefs fs usage <mount>` invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BcachefsInfo {
    pub devices: Vec<BcachefsDevice>,
    /// Filesystem size from the `Size:` line.
    pub size: u64,
    /// Replica levels in use and the data stored at each, e.g. `(1, bytes)`.
    pub replicas: Vec<(u8, u64)>,
    /// True when the output reports degraded data.
    pub degraded: bool,
}

impl From<BcachefsInfo> for ArrayInfo {
    fn from(info: BcachefsInfo) -> Self {
        // The level holding the most data describes the array's real
        // redundancy; a little replicated metadata should not make RAID0
        // look like 2x.
        let redundancy = info
            .replicas
            .iter()
            .max_by_key(|(_, bytes)| *bytes)
            .map(|(level, _)| format!("{level}x"));
        Self {
            devices: info
                .devices
                .into_iter()
                .map(|device| ArrayDevice {
                    label: device.label,
                    device: device.device,
                    state: device.state,
                    size: device.size,
                    used: device.used,
                    use_percent: device.use_percent,
                })
                .collect(),
            redundancy,
            degraded: info.degraded,
            kind: ArrayKind::Bcachefs,
        }
    }
}

/// Parse `bcachefs fs usage` output.
///
/// Returns `None` when no device table is found, so a failed or unrelated
/// command never produces an empty array section in the UI.
pub fn parse_bcachefs_usage(output: &str) -> Option<BcachefsInfo> {
    let mut info = BcachefsInfo::default();
    let mut in_device_table = false;
    // Column offsets of the `-1x`/`-2x` degradation columns in the
    // replication matrix, relative to the `undegraded` column.
    let mut degraded_columns: Vec<usize> = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Each replication matrix has an `undegraded` column plus one column
        // per degraded state. A bare `undegraded` line (older format) is just
        // the healthy case with no degradation columns. There is no standalone
        // "degraded" line in real output, so health is derived from the values
        // in the degradation columns below. Never clear `degraded` here: a
        // later (e.g. erasure-coded) section must not hide earlier degradation.
        if !in_device_table && trimmed.contains("undegraded") {
            degraded_columns = degraded_column_offsets(trimmed).unwrap_or_default();
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("Size:") {
            if info.size == 0 {
                info.size = value
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
            }
            continue;
        }

        if trimmed.starts_with("Device label") {
            in_device_table = true;
            continue;
        }

        if !in_device_table {
            // Replica lines look like "1x:  519473261568", or in degraded
            // output "2x:  311385128960  10737418240" (undegraded, -1x, ...).
            // Erasure-coded lines use levels like "2+1" and are not recorded
            // in `replicas`, but their degradation columns still matter.
            let Some((kind, value)) = trimmed.split_once(':') else {
                continue;
            };
            let level = kind.trim_end_matches('x').parse::<u8>().ok();
            if level.is_none() && !is_erasure_level(kind) {
                continue;
            }
            let values: Vec<&str> = value.split_whitespace().collect();
            if let Some(level) = level {
                let bytes = values.first().and_then(|v| parse_size(v)).unwrap_or(0);
                info.replicas.push((level, bytes));
            }
            // A non-zero count in any degradation column means data is
            // actually degraded; a listed-but-zero column is healthy.
            if degraded_columns.iter().any(|&col| {
                values
                    .get(col)
                    .and_then(|v| parse_size(v))
                    .is_some_and(|bytes| bytes > 0)
            }) {
                info.degraded = true;
            }
            continue;
        }

        if let Some(device) = parse_device_row(trimmed) {
            info.devices.push(device);
        }
    }

    (!info.devices.is_empty()).then_some(info)
}

/// Return the offsets of degradation columns in a replication-matrix header,
/// counted from the `undegraded` column so they align with replica row values.
fn degraded_column_offsets(line: &str) -> Option<Vec<usize>> {
    let columns: Vec<&str> = line.split_whitespace().collect();
    let undegraded = columns
        .iter()
        .position(|col| col.trim_end_matches(':') == "undegraded")?;
    Some(
        columns
            .iter()
            .enumerate()
            .filter(|(idx, col)| *idx > undegraded && is_degraded_column(col))
            .map(|(idx, _)| idx - undegraded)
            .collect(),
    )
}

/// Erasure-coded row levels look like `2+1` or `2+2`. They cannot be stored in
/// `replicas` (a `u8` cannot represent them), but their values still need the
/// degradation-column check.
fn is_erasure_level(kind: &str) -> bool {
    let parts: Vec<&str> = kind.trim().split('+').collect();
    parts.len() > 1
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

/// Degradation column labels include `-1x`, `-2x` and erasure-coded variants
/// such as `-1+1`; plain negative numbers are not column labels.
fn is_degraded_column(token: &str) -> bool {
    let Some(rest) = token.trim_end_matches(':').strip_prefix('-') else {
        return false;
    };
    rest.chars().any(|c| c.is_ascii_digit())
        && rest.chars().any(|c| c.is_ascii_alphabetic() || c == '+')
}

/// Parse a row of the device table. Labels can contain spaces and always end
/// with `:` (`ultra_fast (device 2):`), so the first colon-terminated token
/// marks the end of the label.
fn parse_device_row(line: &str) -> Option<BcachefsDevice> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let label_end = fields.iter().position(|field| field.ends_with(':'))?;
    // label + device + state + size + used + use%
    if fields.len() < label_end + 6 {
        return None;
    }

    let size = fields[label_end + 3].parse().ok()?;
    let used = fields[label_end + 4].parse().ok()?;

    Some(BcachefsDevice {
        label: fields[..=label_end]
            .join(" ")
            .trim_end_matches(':')
            .to_string(),
        device: fields[label_end + 1].to_string(),
        state: fields[label_end + 2].to_string(),
        size,
        used,
        use_percent: fields[label_end + 5].trim_end_matches('%').parse().ok(),
        leaving: fields
            .get(label_end + 6)
            .and_then(|value| value.parse().ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Filesystem: 40178a03-bac6-4bf1-8e72-b3b1050aa22b
Size:             2003687766528  
Used:              525418951680  
Free:             1455529184256  1201015651840  writable now
  at 2 replicas:  1309072500224   829454684160  

       undegraded  
1x:  519473261568  
2x:    5919211520  

Device label            Device     State          Size          Used  Use%    Leaving  
sata_ssd (device 6):    sda        rw     254057377792   98936097280   39%  
slow (device 1):        sdb        rw     996178653184  102638012928   11%  
ultra_fast (device 2):  nvme0n1p2  rw     464425840640  241733880832   56%  429785088  
ultra_fast (device 3):  nvme1n1p1  rw     464425840640  241873883136   56%  426815488  
";

    #[test]
    fn parses_multi_device_four_hosts() {
        let info = parse_bcachefs_usage(SAMPLE).expect("device table");
        assert_eq!(info.size, 2_003_687_766_528);
        assert!(!info.degraded);
        assert_eq!(
            info.replicas,
            vec![(1, 519_473_261_568), (2, 5_919_211_520)]
        );
        assert_eq!(info.devices.len(), 4);
        assert_eq!(
            info.devices[0],
            BcachefsDevice {
                label: "sata_ssd (device 6)".into(),
                device: "sda".into(),
                state: "rw".into(),
                size: 254_057_377_792,
                used: 98_936_097_280,
                use_percent: Some(39),
                leaving: None,
            }
        );
        assert_eq!(info.devices[2].label, "ultra_fast (device 2)");
        assert_eq!(info.devices[2].leaving, Some(429_785_088));
    }

    // Older bcachefs format: degradation is reported via a `-1x` matrix
    // column, never via a standalone "degraded" line.
    const DEGRADED_SAMPLE: &str = "\
Filesystem: 7a3f9c12-4b8e-4d1a-9f2b-8c6e5d4a3b21
Size:                  2147483648000
Used:                   536870912000
Online reserved:            16777216

Data by durability desired and amount degraded:
          undegraded           -1x
    1x: 214748364800             0
    2x: 311385128960   10737418240

btree:          1/2             2             [nvme0n1 nvme1n1]      1073741824

Device label            Device     State          Size          Used  Use%
nvme0n1 (device 1):     nvme0n1    rw     1000204886016  500102444008  50%
nvme1n1 (device 2):     nvme1n1    offline 1000204886016              0   0%
";

    #[test]
    fn degraded_output_is_reported() {
        let info = parse_bcachefs_usage(DEGRADED_SAMPLE).expect("device table");
        assert!(info.degraded);
        assert_eq!(
            info.replicas,
            vec![(1, 214_748_364_800), (2, 311_385_128_960)]
        );
        assert_eq!(info.devices[1].state, "offline");
    }

    #[test]
    fn degradation_column_with_only_zero_values_is_healthy() {
        let output = DEGRADED_SAMPLE.replace(
            "2x: 311385128960   10737418240",
            "2x: 311385128960              0",
        );
        let info = parse_bcachefs_usage(&output).expect("device table");
        assert!(!info.degraded);
        assert_eq!(
            info.replicas,
            vec![(1, 214_748_364_800), (2, 311_385_128_960)]
        );
    }

    #[test]
    fn two_degradation_columns_are_checked_by_position() {
        // Newer format: only the `-2x` column has a non-zero value.
        let output = "\
Replicated:
                    undegraded         -1x         -2x
  1x:                      40           0           0
  2x:                       0           0   10737418240

Device label            Device     State          Size          Used  Use%
nvme0n1 (device 1):     nvme0n1    rw     1000204886016  500102444008  50%
";
        let info = parse_bcachefs_usage(output).expect("device table");
        assert!(info.degraded);
        assert_eq!(info.replicas, vec![(1, 40), (2, 0)]);
    }

    #[test]
    fn human_readable_degradation_values_are_detected() {
        let output = "\
Replicated:
                    undegraded         -1x         -2x
  1x:                    40.4G       3.84T           0
  2x:                        0       1.12T           0

Device label            Device     State          Size          Used  Use%
nvme0n1 (device 1):     nvme0n1    rw     1000204886016  500102444008  50%
";
        let info = parse_bcachefs_usage(output).expect("device table");
        assert!(info.degraded);
        assert_eq!(info.replicas[0].1, (40.4 * 1_024f64.powi(3)) as u64);
    }

    #[test]
    fn later_healthy_erasure_section_does_not_clear_degraded() {
        let output = "\
Replicated:
                    undegraded         -1x         -2x
  1x:                    40.4G       3.84T           0
  2x:                        0       1.12T           0

Erasure coded (data+parity):
                    undegraded         -1x         -2x
  2+1:                   6.08T           0           0
  2+2:                   4.26T           0           0

Device label            Device     State          Size          Used  Use%
nvme0n1 (device 1):     nvme0n1    rw     1000204886016  500102444008  50%
";
        let info = parse_bcachefs_usage(output).expect("device table");
        assert!(info.degraded);
        assert_eq!(info.replicas.len(), 2);
    }

    #[test]
    fn erasure_coded_degradation_is_reported() {
        let output = "\
Replicated:
                    undegraded         -1x         -2x
  1x:                      100           0           0

Erasure coded (data+parity):
                    undegraded         -1x         -2x
  2+1:                     200         512           0
  2+2:                     300           0           0

Device label            Device     State          Size          Used  Use%
nvme0n1 (device 1):     nvme0n1    rw     1000204886016  500102444008  50%
";
        let info = parse_bcachefs_usage(output).expect("device table");
        assert!(info.degraded);
        // Erasure levels cannot be stored as a u8, so replicas stay unchanged.
        assert_eq!(info.replicas, vec![(1, 100)]);
    }

    #[test]
    fn non_replication_rows_do_not_mark_degraded() {
        let output = "\
Data by durability desired and amount degraded:
          undegraded           -1x
    1x: 100 0
btree:          1/2             2             [nvme0n1 nvme1n1]      1073741824
Free:          1000        999999

Device label            Device     State          Size          Used  Use%
nvme0n1 (device 1):     nvme0n1    rw     1000204886016  500102444008  50%
";
        let info = parse_bcachefs_usage(output).expect("device table");
        assert!(!info.degraded);
        assert_eq!(info.replicas, vec![(1, 100)]);
    }

    #[test]
    fn bare_degraded_label_is_not_a_health_signal() {
        // Regression guard: fabricated "degraded" lines used to make the old
        // test pass while real degraded output was still reported as healthy.
        let output = SAMPLE.replace("       undegraded", "          degraded");
        let info = parse_bcachefs_usage(&output).expect("device table");
        assert!(!info.degraded);
    }

    #[test]
    fn output_without_device_table_is_rejected() {
        assert!(parse_bcachefs_usage("bcachefs: command not found").is_none());
        assert!(parse_bcachefs_usage("Size: 1000\nUsed: 500\n").is_none());
    }

    #[test]
    fn device_rows_with_missing_optional_columns_are_accepted() {
        let output = "Device label  Device  State  Size  Used  Use%\nsingle: sda rw 100 50 50%\n";
        let info = parse_bcachefs_usage(output).expect("device table");
        assert_eq!(info.devices.len(), 1);
        assert_eq!(info.devices[0].label, "single");
        assert_eq!(info.devices[0].use_percent, Some(50));
        assert_eq!(info.devices[0].leaving, None);
    }
}
