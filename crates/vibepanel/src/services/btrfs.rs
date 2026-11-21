//! Parsing of btrfs multi-device array details.
//!
//! `btrfs filesystem show <mount>` provides the per-device table and reports
//! missing devices; `btrfs filesystem usage -b <mount>` provides the data
//! profile (RAID level) and a second degraded signal. Running the commands and
//! deciding when to refresh them belongs to `SystemService`.

use super::array::{ArrayDevice, ArrayInfo, ArrayKind, parse_size};

/// One device from `btrfs filesystem show`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BtrfsDevice {
    pub devid: u64,
    /// Device path, or `missing` when the device is absent.
    pub path: String,
    /// `rw` for a present device, `missing` otherwise.
    pub state: String,
    pub size: u64,
    pub used: u64,
    pub use_percent: Option<u32>,
}

/// Array details gathered from the two btrfs commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BtrfsInfo {
    pub devices: Vec<BtrfsDevice>,
    /// Data profile such as `RAID1`; `None` for a single/unknown profile.
    pub redundancy: Option<String>,
    pub degraded: bool,
}

impl From<BtrfsInfo> for ArrayInfo {
    fn from(info: BtrfsInfo) -> Self {
        Self {
            devices: info
                .devices
                .into_iter()
                .map(|device| ArrayDevice {
                    label: format!("devid {}", device.devid),
                    device: device.path,
                    state: device.state,
                    size: device.size,
                    used: device.used,
                    use_percent: device.use_percent,
                })
                .collect(),
            redundancy: info.redundancy,
            degraded: info.degraded,
            kind: ArrayKind::Btrfs,
        }
    }
}

/// Parse `btrfs filesystem show <mount>`.
///
/// Returns `None` when no device lines are found, so a failed or unrelated
/// command never produces an empty array section in the UI.
pub fn parse_btrfs_show(output: &str) -> Option<BtrfsInfo> {
    let mut devices = Vec::new();
    let mut total_devices = None;
    let mut degraded = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("devid") {
            if let Some(device) = parse_device_line(trimmed) {
                if device.state != "rw" {
                    degraded = true;
                }
                devices.push(device);
            }
        } else if let Some(value) = trimmed.strip_prefix("Total devices") {
            total_devices = value.split_whitespace().next().and_then(|v| v.parse().ok());
        } else if trimmed.contains("Some devices missing") {
            degraded = true;
        }
    }

    if devices.is_empty() {
        return None;
    }
    // btrfs omits the line of a missing device in some versions, but the
    // "Total devices" count still includes it.
    if total_devices.is_some_and(|total| devices.len() < total) {
        degraded = true;
    }

    Some(BtrfsInfo {
        devices,
        redundancy: None,
        degraded,
    })
}

/// Add the details from `btrfs filesystem usage -b <mount>` to an existing show
/// result. Missing do not necessarily appear in both commands, so degradation
/// is combined rather than overwritten.
pub fn merge_btrfs_usage(info: &mut BtrfsInfo, output: &str) {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("Device missing:") {
            if value
                .split_whitespace()
                .next()
                .and_then(parse_size)
                .is_some_and(|bytes| bytes > 0)
            {
                info.degraded = true;
            }
            continue;
        }
        if let Some(profile) = trimmed.strip_prefix("Data,") {
            let profile = profile.split(':').next().unwrap_or("").trim();
            if !profile.is_empty() && profile != "single" {
                info.redundancy = Some(profile.to_string());
            }
        }
    }
}

/// Parse one `devid ... size ... used ... path ...` line. Field lookup by name
/// keeps this working across btrfs-progs versions that vary the spacing and
/// column order.
fn parse_device_line(line: &str) -> Option<BtrfsDevice> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let devid = *fields.get(1)?;
    let devid = devid.parse().ok()?;
    let size = field_value(&fields, "size").and_then(parse_size)?;
    let used = field_value(&fields, "used").and_then(parse_size)?;
    let path = field_value(&fields, "path")?.to_string();
    let missing = path == "missing";
    let use_percent = (size > 0 && !missing)
        .then(|| (used.saturating_mul(100) / size).min(u64::from(u32::MAX)) as u32);

    Some(BtrfsDevice {
        devid,
        state: if missing { "missing" } else { "rw" }.to_string(),
        path,
        size,
        used,
        use_percent,
    })
}

fn field_value<'a>(fields: &[&'a str], name: &str) -> Option<&'a str> {
    let index = fields.iter().position(|field| *field == name)?;
    fields.get(index + 1).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOW: &str = "\
Label: 'StoragePool'  uuid: 8a9b7c6d-1234-4321-abcd-ef0123456789
        Total devices 3 FS bytes used 1.46TiB
        devid    1 size 2.73TiB used 1.29TiB path /dev/sdc1
        devid    2 size 223.57GiB used 209.03GiB path /dev/sde
        devid    3 size 5.28TiB used 1.46TiB path /dev/sdd1
";

    const USAGE: &str = "\
Overall:
    Device size:                 8999962181632
    Device missing:                          0
    Used:                        3170142535680

Data,RAID1: Size:3166732943360, Used:3165104422912 (99.95%)
   /dev/sdc1     1415577600000
   /dev/sde       224412041216

Metadata,RAID1: Size:3221225472, Used:2519056384 (78.20%)
   /dev/sdc1        1073741824
";

    #[test]
    fn parses_healthy_multi_device_show() {
        let info = parse_btrfs_show(SHOW).expect("device table");
        assert!(!info.degraded);
        assert_eq!(info.devices.len(), 3);
        assert_eq!(info.devices[0].devid, 1);
        assert_eq!(info.devices[0].path, "/dev/sdc1");
        assert_eq!(info.devices[0].size, (2.73 * 1024f64.powi(4)) as u64);
        assert_eq!(info.devices[1].state, "rw");
        assert!(
            info.devices[1]
                .use_percent
                .is_some_and(|p| (92..=95).contains(&p))
        );
    }

    #[test]
    fn inline_missing_device_is_degraded() {
        let output = SHOW.replace(
            "devid    2 size 223.57GiB used 209.03GiB path /dev/sde",
            "devid    2 size 0 used 0 path missing",
        );
        let info = parse_btrfs_show(&output).expect("device table");
        assert!(info.degraded);
        assert_eq!(info.devices[1].state, "missing");
        assert_eq!(info.devices[1].use_percent, None);
    }

    #[test]
    fn omitted_device_line_is_degraded() {
        let output = SHOW.replace(
            "        devid    2 size 223.57GiB used 209.03GiB path /dev/sde\n",
            "",
        );
        let info = parse_btrfs_show(&output).expect("device table");
        assert!(info.degraded);
        assert_eq!(info.devices.len(), 2);
    }

    #[test]
    fn some_devices_missing_marker_is_degraded() {
        let output = format!("{SHOW}        *** Some devices missing\n");
        let info = parse_btrfs_show(&output).expect("device table");
        assert!(info.degraded);
    }

    #[test]
    fn usage_adds_profile_and_degradation() {
        let mut info = parse_btrfs_show(SHOW).expect("device table");
        merge_btrfs_usage(&mut info, USAGE);
        assert_eq!(info.redundancy.as_deref(), Some("RAID1"));
        assert!(!info.degraded);

        let degraded_usage = USAGE.replace(
            "Device missing:                          0",
            "Device missing:                  1000204886016",
        );
        merge_btrfs_usage(&mut info, &degraded_usage);
        assert!(info.degraded);
    }

    #[test]
    fn single_profile_has_no_redundancy() {
        let mut info = parse_btrfs_show(SHOW).expect("device table");
        let output = USAGE.replace("Data,RAID1:", "Data,single:");
        merge_btrfs_usage(&mut info, &output);
        assert_eq!(info.redundancy, None);
    }

    #[test]
    fn unrelated_output_is_rejected() {
        assert!(parse_btrfs_show("btrfs: command not found").is_none());
        assert!(parse_btrfs_show("Label: 'x' uuid: y\nTotal devices 2\n").is_none());
    }
}
