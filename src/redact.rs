//! Scrubbing identifiers out of system context.
//!
//! Everything Oracle gathers passes through here before it is shown to a model
//! or written to a report. On the default loopback setup the text never leaves
//! the machine, so this is belt-and-braces -- but the endpoint is
//! configurable, `oracle doctor --json` output gets pasted into forum threads,
//! and a log line is exactly the kind of thing that carries a WPA passphrase
//! or a bearer token by accident.
//!
//! The rules are deliberately blunt. Losing a MAC address costs a diagnosis
//! nothing; leaking one costs the user something.

/// Replace identifying detail in `input`.
pub fn scrub(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for (i, line) in input.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&scrub_line(line));
    }
    if input.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn scrub_line(line: &str) -> String {
    // A line whose key names a secret loses its whole value: no attempt is
    // made to keep a useful prefix, because a partial key is still a key.
    if let Some(masked) = mask_secret_assignment(line) {
        return masked;
    }
    let s = mask_home(line);
    let s = mask_user(&s);
    let s = mask_tokens(&s);
    let s = mask_emails(&s);
    let s = mask_macs(&s);
    mask_public_ips(&s)
}

const SECRET_KEYS: [&str; 14] = [
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
    "credential",
    "auth",
    "bearer",
    "psk",
    "pre-shared",
];

fn mask_secret_assignment(line: &str) -> Option<String> {
    let lower = line.to_ascii_lowercase();
    let sep = line.find(['=', ':'])?;
    let key = &lower[..sep];
    // Match on a word boundary so `authoritative` or `tokenizer` do not trip it.
    let hit = SECRET_KEYS.iter().any(|k| {
        key.match_indices(k).any(|(at, _)| {
            let before_ok = at == 0 || !key.as_bytes()[at - 1].is_ascii_alphanumeric();
            let end = at + k.len();
            let after_ok = end == key.len() || !key.as_bytes()[end].is_ascii_alphanumeric();
            before_ok && after_ok
        })
    });
    if !hit {
        return None;
    }
    let value = line[sep + 1..].trim();
    if value.is_empty() {
        return None;
    }
    Some(format!(
        "{}{} [redacted]",
        &line[..sep],
        &line[sep..sep + 1]
    ))
}

fn mask_home(line: &str) -> String {
    let home = crate::config::home();
    let home = home.to_string_lossy();
    if home.len() > 1 && line.contains(home.as_ref()) {
        line.replace(home.as_ref(), "~")
    } else {
        line.to_string()
    }
}

fn mask_user(line: &str) -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default();
    // Very short names would match far too much ordinary text.
    if user.len() < 3 {
        return line.to_string();
    }
    replace_word(line, &user, "[user]")
}

/// Replace `needle` only where it stands as its own word.
fn replace_word(haystack: &str, needle: &str, with: &str) -> String {
    let mut out = String::with_capacity(haystack.len());
    let bytes = haystack.as_bytes();
    let mut i = 0;
    while let Some(rel) = haystack[i..].find(needle) {
        let at = i + rel;
        let end = at + needle.len();
        let before_ok = at == 0 || !is_wordish(bytes[at - 1]);
        let after_ok = end == bytes.len() || !is_wordish(bytes[end]);
        out.push_str(&haystack[i..at]);
        if before_ok && after_ok {
            out.push_str(with);
        } else {
            out.push_str(needle);
        }
        i = end;
    }
    out.push_str(&haystack[i..]);
    out
}

fn is_wordish(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Long opaque runs of base64/hex are keys far more often than they are
/// anything a diagnosis needs.
fn mask_tokens(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for (i, tok) in line.split_inclusive(char::is_whitespace).enumerate() {
        let _ = i;
        let trimmed = tok.trim_end();
        let trail = &tok[trimmed.len()..];
        let core = trimmed.trim_matches(|c: char| "\"'`,;()[]{}<>".contains(c));
        if looks_like_a_key(core) {
            let at = trimmed.find(core).unwrap_or(0);
            out.push_str(&trimmed[..at]);
            out.push_str("[redacted]");
            out.push_str(&trimmed[at + core.len()..]);
        } else {
            out.push_str(trimmed);
        }
        out.push_str(trail);
    }
    out
}

fn looks_like_a_key(s: &str) -> bool {
    if s.len() < 28 {
        return false;
    }
    // A path or a URL is long and opaque but is not a secret, and is often the
    // single most useful thing in a log line.
    if s.contains('/') || s.contains("://") {
        return false;
    }
    let mut digits = 0usize;
    let mut uppers = 0usize;
    let mut lowers = 0usize;
    for c in s.chars() {
        match c {
            '0'..='9' => digits += 1,
            'A'..='Z' => uppers += 1,
            'a'..='z' => lowers += 1,
            '+' | '/' | '=' | '_' | '-' | '.' => {}
            _ => return false,
        }
    }
    // Mixed case plus digits, or a long pure-hex run: both say "generated",
    // not "written by a person".
    let mixed = uppers > 0 && lowers > 0 && digits > 0;
    let hexish = s.len() >= 32 && s.chars().all(|c| c.is_ascii_hexdigit());
    mixed || hexish
}

fn mask_emails(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for tok in line.split_inclusive(char::is_whitespace) {
        let trimmed = tok.trim_end();
        let trail = &tok[trimmed.len()..];
        let core = trimmed.trim_matches(|c: char| "\"'`,;<>()[]".contains(c));
        let is_email = core.contains('@')
            && !core.starts_with('@')
            && core
                .rsplit('@')
                .next()
                .map(|d| d.contains('.') && !d.ends_with('.'))
                .unwrap_or(false);
        if is_email {
            let at = trimmed.find(core).unwrap_or(0);
            out.push_str(&trimmed[..at]);
            out.push_str("[email]");
            out.push_str(&trimmed[at + core.len()..]);
        } else {
            out.push_str(trimmed);
        }
        out.push_str(trail);
    }
    out
}

fn mask_macs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for tok in line.split_inclusive(|c: char| c.is_whitespace() || c == ',') {
        let trimmed = tok.trim_end_matches(|c: char| c.is_whitespace() || c == ',');
        let trail = &tok[trimmed.len()..];
        if is_mac(trimmed) {
            out.push_str("[mac]");
        } else {
            out.push_str(trimmed);
        }
        out.push_str(trail);
    }
    out
}

fn is_mac(s: &str) -> bool {
    let parts: Vec<&str> = s.split(&[':', '-'][..]).collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Private and loopback addresses stay: they are what makes a network
/// diagnosis possible, and they say nothing about where you are. Routable
/// addresses go, IPv4 and IPv6 alike.
///
/// One exception to "private stays": an IPv6 interface identifier built from
/// the MAC (EUI-64, the `ff:fe` in the middle of the second half) is masked on
/// any address, link-local included, because it *is* the MAC.
fn mask_public_ips(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for tok in line.split_inclusive(|c: char| c.is_whitespace() || c == ',') {
        let trimmed = tok.trim_end_matches(|c: char| c.is_whitespace() || c == ',');
        let trail = &tok[trimmed.len()..];
        let core = trimmed.trim_matches(|c: char| "\"'`()[]".contains(c));
        match mask_address(core) {
            Some(masked) => {
                let at = trimmed.find(core).unwrap_or(0);
                out.push_str(&trimmed[..at]);
                out.push_str(&masked);
                out.push_str(&trimmed[at + core.len()..]);
            }
            None => out.push_str(trimmed),
        }
        out.push_str(trail);
    }
    out
}

/// The masked form of `core` when it holds an address worth hiding, keeping
/// whatever follows the address -- a port, a prefix length, a zone. `None`
/// means leave it as it is.
fn mask_address(core: &str) -> Option<String> {
    // IPv4: keep any /prefix or :port suffix out of the parse.
    let bare = core.split(['/', ':']).next().unwrap_or(core);
    if let Ok(ip) = bare.parse::<std::net::Ipv4Addr>() {
        return is_public_v4(&ip).then(|| format!("[ip]{}", &core[bare.len()..]));
    }

    // IPv6: the address ends at a zone (`%wlan0`), a prefix length (`/64`),
    // or the bracket that comes before a port (`[2001:…]:443`).
    let end = core.find([']', '/', '%']).unwrap_or(core.len());
    let ip = core[..end].parse::<std::net::Ipv6Addr>().ok()?;
    let rest = &core[end..];
    if is_public_v6(&ip) {
        return Some(format!("[ip]{rest}"));
    }
    if has_eui64_interface_id(&ip) {
        // Keep the network half, which is what a diagnosis uses.
        let network = std::net::Ipv6Addr::from(u128::from(ip) & !(u128::from(u64::MAX)));
        return Some(format!("{network}[eui64]{rest}"));
    }
    None
}

fn is_public_v6(ip: &std::net::Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(&v4);
    }
    let s = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        // 2001:db8::/32, the documentation prefix.
        || (s[0] == 0x2001 && s[1] == 0x0db8))
}

/// Whether the interface identifier was derived from a MAC address: EUI-64
/// puts `ff:fe` in the middle of it.
fn has_eui64_interface_id(ip: &std::net::Ipv6Addr) -> bool {
    let o = ip.octets();
    o[11] == 0xff && o[12] == 0xfe
}

fn is_public_v4(ip: &std::net::Ipv4Addr) -> bool {
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_documentation()
        || ip.is_multicast()
        // Carrier-grade NAT, 100.64.0.0/10: a home router's address, not a
        // public one.
        || (ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wifi_passphrase_does_not_survive() {
        let out = scrub("psk=hunter2correcthorse");
        assert!(!out.contains("hunter2"), "got {out}");
        assert!(out.contains("[redacted]"));
    }

    #[test]
    fn a_key_shaped_word_is_masked_but_a_path_is_not() {
        let out = scrub("token aB3dEfGh1jKlMn0pQrStUvWxYz12 loaded");
        assert!(out.contains("[redacted]"), "got {out}");

        let keep = "/usr/lib/firmware/rtl_bt/rtl8821c_fw.bin";
        assert!(scrub(keep).contains(keep), "a path must stay readable");
    }

    #[test]
    fn private_addresses_stay_and_public_ones_go() {
        let out = scrub("route via 192.168.1.1 to 93.184.216.34");
        assert!(out.contains("192.168.1.1"), "got {out}");
        assert!(!out.contains("93.184.216.34"), "got {out}");
    }

    #[test]
    fn a_port_survives_a_masked_address() {
        let out = scrub("connect 93.184.216.34:443");
        assert!(out.contains("[ip]:443"), "got {out}");
    }

    #[test]
    fn a_public_ipv6_address_goes_and_keeps_its_prefix_length() {
        // The shape `ip addr` and the network probe actually produce, taken
        // from a real machine where this once leaked.
        let out = scrub(
            "wlp1s0: up, addresses: 192.168.1.134/24 \
             2600:1700:1241:6300:5e8a:aeff:feaf:5d1a/64 fe80::5e8a:aeff:feaf:5d1a/64",
        );
        assert!(!out.contains("2600:1700"), "got {out}");
        assert!(!out.contains("5e8a"), "the MAC-derived half must go: {out}");
        assert!(out.contains("192.168.1.134/24"), "got {out}");
        assert!(out.contains("[ip]/64"), "got {out}");
        assert!(out.contains("fe80::[eui64]/64"), "got {out}");
    }

    #[test]
    fn private_and_loopback_ipv6_addresses_stay() {
        for line in [
            "Default route via fe80::1 on wlp1s0",
            "listening on ::1 port 11434",
            "ula fd12:3456:789a:1::42/64",
            "example 2001:db8::7",
            "multicast ff02::1",
        ] {
            assert_eq!(scrub(line), line);
        }
    }

    #[test]
    fn a_mac_derived_address_is_masked_even_when_link_local() {
        let out = scrub("neighbour fe80::5e8a:aeff:feaf:5d1a%wlp1s0");
        assert_eq!(out, "neighbour fe80::[eui64]%wlp1s0");
    }

    #[test]
    fn a_bracketed_ipv6_address_loses_the_address_and_keeps_the_port() {
        let out = scrub("connect [2606:4700::1111]:443");
        assert!(!out.contains("2606"), "got {out}");
        assert!(out.ends_with(":443"), "got {out}");
    }

    #[test]
    fn an_ipv4_mapped_address_follows_the_ipv4_rules() {
        assert!(!scrub("peer ::ffff:93.184.216.34").contains("93.184"));
        let private = "peer ::ffff:192.168.1.1";
        assert_eq!(scrub(private), private);
    }

    #[test]
    fn colon_heavy_text_that_is_not_an_address_is_left_alone() {
        for line in [
            "23:52:38.944 started",
            "at std::net::Ipv6Addr::from",
            "key: value",
        ] {
            assert_eq!(scrub(line), line);
        }
    }

    #[test]
    fn mac_addresses_go() {
        let out = scrub("wlan0 link/ether a4:5e:60:bb:1c:9f brd ff:ff:ff:ff:ff:ff");
        assert!(!out.contains("a4:5e:60"), "got {out}");
        assert_eq!(out.matches("[mac]").count(), 2);
    }

    #[test]
    fn ordinary_log_text_is_left_alone() {
        let line = "rvnd: bound /run/rvn/ctl, waiting for wheel";
        assert_eq!(scrub(line), line);
    }

    #[test]
    fn a_word_containing_a_secret_key_name_is_not_a_secret() {
        let line = "authoritative: yes";
        assert_eq!(scrub(line), line);
    }
}
