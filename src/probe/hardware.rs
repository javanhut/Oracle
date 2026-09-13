//! Hardware: the physical constraints behind a slow or unhappy machine.
//!
//! Most of what people call a performance problem is one of four things: the
//! machine is out of memory and swapping, it is thermally throttled, the
//! battery is on a power profile that caps the clock, or a driver never loaded
//! because its firmware is missing. All four are readable from `/proc` and
//! `/sys`, and none of them is obvious from the desktop.

use serde::Serialize;

#[derive(Debug, Default, Serialize)]
pub struct Hardware {
    pub cpu_model: String,
    pub cpu_count: usize,
    pub load: Option<(f32, f32, f32)>,
    pub memory: Memory,
    pub battery: Option<Battery>,
    pub thermal_celsius: Option<f32>,
    pub gpu: Vec<String>,
    /// Firmware files the kernel asked for and did not get. Each one is a
    /// device that silently does not work.
    pub missing_firmware: Vec<String>,
    pub virtualised: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Memory {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Battery {
    pub name: String,
    pub percent: Option<u8>,
    pub status: String,
    /// Full-charge capacity against design capacity: how much of the original
    /// battery is left.
    pub health_percent: Option<u8>,
}

pub fn probe() -> Hardware {
    let mut h = Hardware {
        cpu_count: cpu_count(),
        cpu_model: cpu_model(),
        load: load_average(),
        memory: memory(),
        battery: battery(),
        thermal_celsius: thermal(),
        gpu: gpus(),
        missing_firmware: missing_firmware(),
        virtualised: virtualisation(),
    };
    h.gpu.dedup();
    h
}

fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0)
}

fn cpu_model() -> String {
    let Some(text) = crate::sys::read("/proc/cpuinfo") else {
        return String::new();
    };
    for line in text.lines() {
        // x86 says "model name"; arm64 says "CPU implementer" and friends, so
        // fall back to the hardware line there.
        for key in ["model name", "Hardware", "Model"] {
            if let Some(rest) = line.strip_prefix(key)
                && let Some(v) = rest.split_once(':')
            {
                let v = v.1.trim();
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    String::new()
}

fn load_average() -> Option<(f32, f32, f32)> {
    let text = crate::sys::read("/proc/loadavg")?;
    let mut it = text.split_whitespace();
    Some((
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    ))
}

fn memory() -> Memory {
    Memory {
        total_bytes: crate::sys::meminfo("MemTotal").unwrap_or(0),
        // MemAvailable, not MemFree: free memory on a healthy Linux is near
        // zero by design, and reporting it as pressure is the single most
        // common way a monitoring tool frightens people for no reason.
        available_bytes: crate::sys::meminfo("MemAvailable").unwrap_or(0),
        swap_total_bytes: crate::sys::meminfo("SwapTotal").unwrap_or(0),
        swap_free_bytes: crate::sys::meminfo("SwapFree").unwrap_or(0),
    }
}

fn battery() -> Option<Battery> {
    let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in entries.flatten() {
        let base = entry.path();
        let kind = crate::sys::read_trimmed(base.join("type")).unwrap_or_default();
        if kind != "Battery" {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        let percent = crate::sys::read_trimmed(base.join("capacity")).and_then(|c| c.parse().ok());
        let status =
            crate::sys::read_trimmed(base.join("status")).unwrap_or_else(|| "unknown".into());

        // Vendors expose either energy_* (µWh) or charge_* (µAh); the ratio is
        // the same either way.
        let health_percent = ["energy", "charge"]
            .iter()
            .find_map(|p| {
                let full: f64 = crate::sys::read_trimmed(base.join(format!("{p}_full")))?
                    .parse()
                    .ok()?;
                let design: f64 = crate::sys::read_trimmed(base.join(format!("{p}_full_design")))?
                    .parse()
                    .ok()?;
                (design > 0.0).then(|| ((full / design) * 100.0).round() as u8)
            })
            .map(|p| p.min(100));

        return Some(Battery {
            name,
            percent,
            status,
            health_percent,
        });
    }
    None
}

/// The hottest thermal zone the kernel exposes, in Celsius.
fn thermal() -> Option<f32> {
    let entries = std::fs::read_dir("/sys/class/thermal").ok()?;
    let mut hottest: Option<f32> = None;
    for entry in entries.flatten() {
        let path = entry.path().join("temp");
        let Some(raw) = crate::sys::read_trimmed(&path) else {
            continue;
        };
        let Ok(milli) = raw.parse::<f32>() else {
            continue;
        };
        let c = milli / 1000.0;
        // Zones sometimes report nonsense; ignore the physically impossible.
        if (0.0..=150.0).contains(&c) {
            hottest = Some(hottest.map_or(c, |h: f32| h.max(c)));
        }
    }
    hottest
}

fn gpus() -> Vec<String> {
    // lspci gives the friendly name when pciutils is installed.
    if let Some(out) = crate::sys::quick("lspci", &[])
        && let Some(text) = out.text()
    {
        let found: Vec<String> = text
            .lines()
            .filter(|l| {
                let low = l.to_ascii_lowercase();
                low.contains("vga compatible")
                    || low.contains("3d controller")
                    || low.contains("display controller")
            })
            .map(|l| {
                l.split_once(": ")
                    .map(|(_, name)| name.trim())
                    .unwrap_or(l.trim())
                    .to_string()
            })
            .collect();
        if !found.is_empty() {
            return found;
        }
    }

    // Without pciutils, the DRM nodes at least say how many there are and
    // which driver bound them.
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("card") && !n.contains('-')
        })
        .filter_map(|e| {
            let driver = std::fs::read_link(e.path().join("device/driver")).ok()?;
            let name = driver.file_name()?.to_string_lossy().into_owned();
            Some(format!(
                "{} (driver {name})",
                e.file_name().to_string_lossy()
            ))
        })
        .collect()
}

/// Firmware the kernel asked for and did not find.
///
/// This is the failure behind most "my wifi/bluetooth/graphics just doesn't
/// exist" reports: the device enumerates, the driver binds, the firmware load
/// fails, and nothing above the kernel ever mentions it.
fn missing_firmware() -> Vec<String> {
    let Some(out) = crate::sys::quick("dmesg", &[]) else {
        return Vec::new();
    };
    let Some(text) = out.text() else {
        return Vec::new();
    };

    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let low = line.to_ascii_lowercase();
        let failed = low.contains("firmware")
            && (low.contains("failed to load")
                || low.contains("direct firmware load")
                || low.contains("no such file"));
        if !failed {
            continue;
        }
        // Pull out the firmware path so the finding names the actual file.
        let name = line
            .split_whitespace()
            .find(|w| w.contains('/') && (w.ends_with(".bin") || w.ends_with(".ucode")))
            .map(|w| {
                w.trim_matches(|c| c == ',' || c == '\'' || c == '"')
                    .to_string()
            })
            .unwrap_or_else(|| line.trim().to_string());
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out.truncate(8);
    out
}

fn virtualisation() -> Option<String> {
    if let Some(out) = crate::sys::quick("systemd-detect-virt", &[])
        && let Some(t) = out.text()
        && t != "none"
        && !t.is_empty()
    {
        return Some(t.to_string());
    }
    let vendor = crate::sys::read_trimmed("/sys/class/dmi/id/sys_vendor")?;
    let low = vendor.to_ascii_lowercase();
    for (needle, name) in [
        ("qemu", "qemu"),
        ("kvm", "kvm"),
        ("vmware", "vmware"),
        ("virtualbox", "virtualbox"),
        ("innotek", "virtualbox"),
        ("microsoft corporation", "hyper-v"),
    ] {
        if low.contains(needle) {
            return Some(name.to_string());
        }
    }
    None
}

impl Memory {
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    pub fn used_percent(&self) -> u8 {
        if self.total_bytes == 0 {
            return 0;
        }
        ((self.used_bytes() as f64 / self.total_bytes as f64) * 100.0).round() as u8
    }

    pub fn swap_used_percent(&self) -> Option<u8> {
        if self.swap_total_bytes == 0 {
            return None;
        }
        let used = self.swap_total_bytes.saturating_sub(self.swap_free_bytes);
        Some(((used as f64 / self.swap_total_bytes as f64) * 100.0).round() as u8)
    }
}

impl Hardware {
    /// Load average per core. Above roughly 1.0 the machine has more runnable
    /// work than it has cores, which is what "slow" usually means.
    pub fn load_per_core(&self) -> Option<f32> {
        let (one, _, _) = self.load?;
        (self.cpu_count > 0).then(|| one / self.cpu_count as f32)
    }

    pub fn on_battery(&self) -> bool {
        self.battery
            .as_ref()
            .map(|b| b.status == "Discharging")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_percentages_use_available_not_free() {
        let m = Memory {
            total_bytes: 16 * 1024 * 1024 * 1024,
            available_bytes: 4 * 1024 * 1024 * 1024,
            swap_total_bytes: 4 * 1024 * 1024 * 1024,
            swap_free_bytes: 1024 * 1024 * 1024,
        };
        assert_eq!(m.used_percent(), 75);
        assert_eq!(m.swap_used_percent(), Some(75));
    }

    #[test]
    fn a_machine_without_swap_reports_no_swap_pressure() {
        let m = Memory {
            total_bytes: 8 * 1024 * 1024 * 1024,
            available_bytes: 8 * 1024 * 1024 * 1024,
            ..Default::default()
        };
        assert_eq!(m.swap_used_percent(), None);
        assert_eq!(m.used_percent(), 0);
    }

    #[test]
    fn load_is_reported_per_core() {
        let h = Hardware {
            cpu_count: 4,
            load: Some((8.0, 4.0, 2.0)),
            ..Default::default()
        };
        assert_eq!(h.load_per_core(), Some(2.0));
    }

    #[test]
    fn the_real_machine_reports_cores_and_memory() {
        let h = probe();
        assert!(h.cpu_count >= 1);
        assert!(h.memory.total_bytes > 0);
    }
}
