//! Network: links, routes, names.
//!
//! Everything here is a passive read of `/proc` and `/sys` unless the caller
//! explicitly asked for online checks. That ordering is on purpose. A tool
//! that silently emits packets to prove the network works is a tool that shows
//! up in someone's firewall log and has to be explained, and the passive reads
//! already answer the common questions: no carrier, no route, no DNS server
//! configured.

use crate::probe::ProbeOptions;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Default, Serialize)]
pub struct Network {
    pub interfaces: Vec<Interface>,
    pub default_route: Option<DefaultRoute>,
    pub nameservers: Vec<String>,
    /// Present only when online checks were permitted.
    pub dns_resolves: Option<bool>,
    pub gateway_reachable: Option<bool>,
    /// Whether CAW, the Raven wireless daemon, is running.
    pub cawd_running: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Interface {
    pub name: String,
    /// `up`, `down`, `dormant`, `unknown`.
    pub operstate: String,
    /// Whether the link sees a peer. `false` on wifi means "not associated".
    pub carrier: bool,
    pub wireless: bool,
    pub loopback: bool,
    pub addresses: Vec<String>,
    pub mtu: Option<u32>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DefaultRoute {
    pub interface: String,
    pub gateway: String,
}

pub fn probe(opts: ProbeOptions) -> Network {
    let mut n = Network {
        interfaces: interfaces(),
        default_route: default_route(),
        nameservers: nameservers(),
        ..Default::default()
    };

    if Path::new("/etc/raven").is_dir() {
        n.cawd_running = Some(process_running("cawd"));
    }

    if opts.online_checks {
        n.dns_resolves = Some(dns_works());
        if let Some(route) = &n.default_route {
            n.gateway_reachable = Some(ping(&route.gateway));
        }
    }

    n
}

fn interfaces() -> Vec<Interface> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };

    let addrs = addresses_by_interface();
    let mut out: Vec<Interface> = Vec::new();

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let base = entry.path();

        let operstate =
            crate::sys::read_trimmed(base.join("operstate")).unwrap_or_else(|| "unknown".into());
        // `carrier` is unreadable while the interface is administratively
        // down, which reads as "no carrier" -- correct for our purposes.
        let carrier = crate::sys::read_trimmed(base.join("carrier"))
            .map(|c| c == "1")
            .unwrap_or(false);
        let wireless = base.join("wireless").exists() || base.join("phy80211").exists();
        let loopback = crate::sys::read_trimmed(base.join("type"))
            .map(|t| t == "772")
            .unwrap_or(name == "lo");

        out.push(Interface {
            operstate,
            carrier,
            wireless,
            loopback,
            addresses: addrs.get(&name).cloned().unwrap_or_default(),
            mtu: crate::sys::read_trimmed(base.join("mtu")).and_then(|m| m.parse().ok()),
            rx_bytes: crate::sys::read_trimmed(base.join("statistics/rx_bytes"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            tx_bytes: crate::sys::read_trimmed(base.join("statistics/tx_bytes"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
            name,
        });
    }

    // Real interfaces first, loopback last; it is never what someone is asking
    // about.
    out.sort_by(|a, b| a.loopback.cmp(&b.loopback).then(a.name.cmp(&b.name)));
    out
}

/// Addresses per interface, from `ip` when it is installed.
///
/// A system without `ip` still gets link state and routes from `/sys` and
/// `/proc`, so this degrades to "no addresses listed" rather than to a blank
/// network section.
fn addresses_by_interface() -> std::collections::HashMap<String, Vec<String>> {
    let mut map: std::collections::HashMap<String, Vec<String>> = Default::default();
    let Some(out) = crate::sys::quick("ip", &["-o", "addr", "show"]) else {
        return map;
    };
    let Some(text) = out.text() else { return map };

    for line in text.lines() {
        // 2: wlan0    inet 192.168.1.24/24 brd ... scope global wlan0
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let iface = cols[1].trim_end_matches(':');
        if cols[2] == "inet" || cols[2] == "inet6" {
            map.entry(iface.to_string())
                .or_default()
                .push(cols[3].to_string());
        }
    }
    map
}

/// The default route, read from `/proc/net/route` so it works without `ip`.
fn default_route() -> Option<DefaultRoute> {
    let text = crate::sys::read("/proc/net/route")?;
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 3 {
            continue;
        }
        // Iface Destination Gateway ...
        if cols[1] != "00000000" {
            continue;
        }
        let Some(gateway) = parse_route_address(cols[2]) else {
            continue;
        };
        return Some(DefaultRoute {
            interface: cols[0].to_string(),
            gateway,
        });
    }
    None
}

/// Decode one address from `/proc/net/route`.
///
/// The kernel writes the address in network byte order and then prints those
/// four bytes as a little-endian hex word, so "FE01A8C0" is 192.168.1.254.
/// Reading the hex into a `u32` and taking its little-endian bytes undoes
/// exactly that, and the first byte out is the first octet -- reversing them
/// again turns a gateway on the local network into an address somewhere in
/// Asia, which is the kind of mistake that survives review because nobody
/// reads a printed IP twice.
fn parse_route_address(hex: &str) -> Option<String> {
    let raw = u32::from_str_radix(hex, 16).ok()?;
    let o = raw.to_le_bytes();
    Some(format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3]))
}

fn nameservers() -> Vec<String> {
    let Some(text) = crate::sys::read("/etc/resolv.conf") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') || l.starts_with(';') {
                return None;
            }
            l.strip_prefix("nameserver").map(|v| v.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether a process with this name is running, by walking `/proc`.
///
/// Cheaper and more reliable than parsing `ps`, and it works identically under
/// raven-init and systemd.
pub fn process_running(name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.join("comm").exists() {
            continue;
        }
        if crate::sys::read_trimmed(path.join("comm")).as_deref() == Some(name) {
            return true;
        }
    }
    false
}

/// A DNS lookup, only ever run when the caller asked for online checks.
fn dns_works() -> bool {
    use std::net::ToSocketAddrs;
    // A name from the distribution's own infrastructure, not a third party's:
    // if this resolves, package operations can resolve too, which is the
    // question actually being asked.
    ("archlinux.org", 443).to_socket_addrs().is_ok()
}

fn ping(host: &str) -> bool {
    crate::sys::run(
        "ping",
        &["-c", "1", "-W", "2", "-n", "--", host],
        std::time::Duration::from_secs(4),
    )
    .map(|o| o.ok())
    .unwrap_or(false)
}

impl Network {
    /// Interfaces that could carry traffic: up, not loopback.
    pub fn usable(&self) -> Vec<&Interface> {
        self.interfaces
            .iter()
            .filter(|i| !i.loopback && i.operstate == "up")
            .collect()
    }

    /// Non-loopback interfaces that exist but are not up.
    pub fn down(&self) -> Vec<&Interface> {
        self.interfaces
            .iter()
            .filter(|i| !i.loopback && i.operstate != "up")
            .collect()
    }

    /// Interfaces that are up but have no address, which is usually a DHCP
    /// failure rather than a link failure.
    pub fn up_without_address(&self) -> Vec<&Interface> {
        self.usable()
            .into_iter()
            .filter(|i| !i.addresses.iter().any(|a| !a.starts_with("fe80")))
            .collect()
    }

    pub fn has_wireless(&self) -> bool {
        self.interfaces.iter().any(|i| i.wireless)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, state: &str, addrs: &[&str]) -> Interface {
        Interface {
            name: name.into(),
            operstate: state.into(),
            carrier: state == "up",
            wireless: name.starts_with("wl"),
            loopback: name == "lo",
            addresses: addrs.iter().map(|s| s.to_string()).collect(),
            mtu: Some(1500),
            rx_bytes: 0,
            tx_bytes: 0,
        }
    }

    #[test]
    fn loopback_is_never_counted_as_usable() {
        let n = Network {
            interfaces: vec![iface("lo", "up", &["127.0.0.1/8"])],
            ..Default::default()
        };
        assert!(n.usable().is_empty());
    }

    #[test]
    fn an_up_link_with_only_a_link_local_address_counts_as_addressless() {
        let n = Network {
            interfaces: vec![iface("wlan0", "up", &["fe80::1/64"])],
            ..Default::default()
        };
        assert_eq!(n.up_without_address().len(), 1);
    }

    #[test]
    fn an_addressed_link_is_not_flagged() {
        let n = Network {
            interfaces: vec![iface("wlan0", "up", &["192.168.1.5/24", "fe80::1/64"])],
            ..Default::default()
        };
        assert!(n.up_without_address().is_empty());
    }

    #[test]
    fn a_route_address_decodes_to_the_gateway_people_actually_have() {
        // 192.168.1.254, as /proc/net/route writes it.
        assert_eq!(
            parse_route_address("FE01A8C0").as_deref(),
            Some("192.168.1.254")
        );
        // 192.168.1.1, the other overwhelmingly common one.
        assert_eq!(
            parse_route_address("0101A8C0").as_deref(),
            Some("192.168.1.1")
        );
        // 10.0.0.1
        assert_eq!(parse_route_address("0100000A").as_deref(), Some("10.0.0.1"));
        // No gateway at all, as an on-link route writes it.
        assert_eq!(parse_route_address("00000000").as_deref(), Some("0.0.0.0"));
    }

    #[test]
    fn a_decoded_gateway_is_a_private_address_on_a_normal_network() {
        // The reversed-byte bug turned every home gateway into a routable
        // address, which redaction then masked -- hiding the bug.
        let gw = parse_route_address("FE01A8C0").unwrap();
        let ip: std::net::Ipv4Addr = gw.parse().unwrap();
        assert!(ip.is_private(), "{gw} should be a private address");
    }

    #[test]
    fn a_malformed_route_line_is_skipped_not_fatal() {
        assert_eq!(parse_route_address("nothex"), None);
    }

    #[test]
    fn the_real_machine_has_a_loopback_interface() {
        let n = probe(ProbeOptions::default());
        assert!(
            n.interfaces.iter().any(|i| i.loopback),
            "every Linux has lo; got {:?}",
            n.interfaces.iter().map(|i| &i.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_passive_probe_performs_no_online_checks() {
        let n = probe(ProbeOptions::default());
        assert_eq!(n.dns_resolves, None);
        assert_eq!(n.gateway_reachable, None);
    }
}
