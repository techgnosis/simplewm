//! Static IPv4 setup for the wired network card, behind the Network applet,
//! plus the DNS server: simplewm owns /etc/resolv.conf and writes the whole
//! file (a comment and one `nameserver` line) when the user applies one.
//!
//! The card's state is read without privileges, from /sys/class/net and
//! rtnetlink dumps. Changing it needs root: `apply` runs one fixed `ip` script
//! through `sudo`, with the user's password (typed into simplewm's password
//! window) on sudo's stdin. The values reach the script only as separate
//! arguments, after `parse` has checked them, never as shell text.

use std::{
    fs,
    io::{self, Write},
    net::Ipv4Addr,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    process::{Command, Stdio},
};

/// An address with its prefix length, the gateway for the default route, and
/// the DNS server for /etc/resolv.conf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Config {
    pub addr: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Option<Ipv4Addr>,
    /// None leaves /etc/resolv.conf as it is.
    pub dns: Option<Ipv4Addr>,
}

/// The file simplewm owns.
pub const RESOLV_CONF: &str = "/etc/resolv.conf";

/// Parse the applet's fields: "192.168.1.50/24", "192.168.1.1" and "1.1.1.1"
/// (the gateway may be empty for no default route, the DNS server empty to
/// leave /etc/resolv.conf alone).
pub fn parse(address: &str, gateway: &str, dns: &str) -> Result<Ipv4Config, String> {
    let address = address.trim();
    let (addr, prefix) = address
        .split_once('/')
        .ok_or("Write the address with its prefix length, like 192.168.1.50/24.")?;
    let addr: Ipv4Addr = addr
        .trim()
        .parse()
        .map_err(|_| format!("\"{}\" is not an IPv4 address.", addr.trim()))?;
    let prefix: u8 = prefix
        .trim()
        .parse()
        .ok()
        .filter(|p| (1..=32).contains(p))
        .ok_or("The prefix length after / must be 1 to 32.")?;
    if addr.is_unspecified() || addr.is_multicast() || addr.is_broadcast() || addr.is_loopback() {
        return Err(format!("{addr} can't be used as the card's address."));
    }
    let mask = mask(prefix);
    let host = u32::from(addr) & !mask;
    if prefix <= 30 && (host == 0 || host == !mask) {
        return Err(format!(
            "{addr} is the network or broadcast address of /{prefix}."
        ));
    }

    let gateway = gateway.trim();
    let gateway = if gateway.is_empty() {
        None
    } else {
        let gw: Ipv4Addr = gateway
            .parse()
            .map_err(|_| format!("\"{gateway}\" is not an IPv4 address."))?;
        if gw == addr {
            return Err("The gateway can't be the card's own address.".into());
        }
        if u32::from(gw) & mask != u32::from(addr) & mask {
            return Err(format!(
                "The gateway {gw} is not inside {}/{prefix}.",
                network(addr, prefix)
            ));
        }
        Some(gw)
    };

    let dns = dns.trim();
    let dns = if dns.is_empty() {
        None
    } else {
        let ns: Ipv4Addr = dns
            .parse()
            .map_err(|_| format!("The DNS server \"{dns}\" is not an IPv4 address."))?;
        if ns.is_unspecified() || ns.is_multicast() || ns.is_broadcast() {
            return Err(format!("{ns} can't be used as a DNS server."));
        }
        Some(ns)
    };
    Ok(Ipv4Config {
        addr,
        prefix,
        gateway,
        dns,
    })
}

/// The whole of /etc/resolv.conf for DNS server `dns`.
pub fn resolv_conf(dns: Ipv4Addr) -> String {
    format!(
        "# simplewm owns this file and rewrites it from its Network applet. Edits here are lost.\nnameserver {dns}\n"
    )
}

/// The `nameserver` lines in /etc/resolv.conf, in order.
pub fn nameservers() -> io::Result<Vec<String>> {
    Ok(parse_nameservers(&fs::read_to_string(RESOLV_CONF)?))
}

fn parse_nameservers(conf: &str) -> Vec<String> {
    conf.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            (words.next() == Some("nameserver"))
                .then(|| words.next())
                .flatten()
                .map(str::to_string)
        })
        .collect()
}

fn mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

fn network(addr: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(addr) & mask(prefix))
}

// --- The card ------------------------------------------------------------------

/// The first physical wired card (Ethernet type, backed by a device, not Wi-Fi).
pub fn ethernet_device() -> Option<String> {
    let mut names: Vec<String> = fs::read_dir("/sys/class/net")
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            let dir = Path::new("/sys/class/net").join(name);
            sysfs(&dir, "type").as_deref() == Some("1")
                && dir.join("device").exists()
                && !dir.join("wireless").exists()
                && !dir.join("phy80211").exists()
        })
        .collect();
    names.sort();
    names.into_iter().next()
}

fn sysfs(dir: &Path, file: &str) -> Option<String> {
    fs::read_to_string(dir.join(file))
        .ok()
        .map(|s| s.trim().to_string())
}

fn ifindex(dev: &str) -> io::Result<u32> {
    sysfs(&Path::new("/sys/class/net").join(dev), "ifindex")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no network card named {dev}"),
            )
        })
}

/// What the applet shows about the card.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub mac: String,
    /// Interface switched on (IFF_UP).
    pub up: bool,
    /// "up", "down", "dormant"... from the kernel.
    pub operstate: String,
    /// Cable plugged in (unknown while the interface is down).
    pub carrier: Option<bool>,
    /// Link speed in Mb/s, if known.
    pub speed: Option<u32>,
    pub addrs: Vec<(Ipv4Addr, u8)>,
    pub gateways: Vec<Ipv4Addr>,
}

pub fn status(dev: &str) -> io::Result<Status> {
    let dir = Path::new("/sys/class/net").join(dev);
    let index = ifindex(dev)?;
    let flags = sysfs(&dir, "flags")
        .and_then(|f| u32::from_str_radix(f.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0);
    let mut nl = Netlink::open()?;
    Ok(Status {
        mac: sysfs(&dir, "address").unwrap_or_default(),
        up: flags & libc::IFF_UP as u32 != 0,
        operstate: sysfs(&dir, "operstate").unwrap_or_default(),
        carrier: sysfs(&dir, "carrier").map(|c| c == "1"),
        speed: sysfs(&dir, "speed")
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|s| *s > 0)
            .map(|s| s as u32),
        addrs: nl
            .addresses(index)?
            .into_iter()
            .map(|a| (a.local, a.prefix))
            .collect(),
        gateways: nl
            .default_routes(index)?
            .into_iter()
            .filter_map(|r| r.gateway)
            .collect(),
    })
}

/// Whether NetworkManager is running (it would undo what simplewm sets).
pub fn network_manager_running() -> bool {
    let Ok(dir) = fs::read_dir("/proc") else {
        return false;
    };
    dir.flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .any(|e| {
            fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "NetworkManager")
        })
}

/// Run as root by `apply`: switch the card on, set the address (keeping it if
/// it is already there, so connections survive), remove the other addresses,
/// replace the default route, and replace /etc/resolv.conf. Arguments: card,
/// address/prefix, gateway (may be empty), the whole new resolv.conf (empty to
/// leave it alone), then the addresses to remove. The file is written beside
/// the old one and renamed over it, so it is never seen half written, and a
/// symlink there (from systemd-resolved, say) is replaced by the file.
const APPLY_SCRIPT: &str = r#"set -e
resolv=/etc/resolv.conf
dev=$1 addr=$2 gw=$3 conf=$4
shift 4
ip link set dev "$dev" up
ip -4 addr replace "$addr" brd + dev "$dev"
for old in "$@"; do ip -4 addr del "$old" dev "$dev"; done
ip -4 route flush exact 0.0.0.0/0 dev "$dev"
if [ -n "$gw" ]; then ip -4 route replace default via "$gw" dev "$dev" proto static; fi
if [ -n "$conf" ]; then
  tmp=$(mktemp "$resolv.simplewm.XXXXXX")
  if ! { printf '%s' "$conf" > "$tmp" && chmod 644 "$tmp" && mv -f "$tmp" "$resolv"; }; then
    rm -f "$tmp"
    echo "cannot write $resolv" >&2
    exit 1
  fi
fi
"#;

/// Why `apply` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// sudo didn't accept the password.
    WrongPassword,
    /// Anything else, explained.
    Failed(String),
}

/// `APPLY_SCRIPT`'s arguments for setting `cfg` on `dev`, which has `current`
/// addresses now.
fn script_args(dev: &str, cfg: &Ipv4Config, current: &[(Ipv4Addr, u8)]) -> Vec<String> {
    let mut args = vec![
        dev.to_string(),
        format!("{}/{}", cfg.addr, cfg.prefix),
        cfg.gateway.map(|g| g.to_string()).unwrap_or_default(),
        cfg.dns.map(resolv_conf).unwrap_or_default(),
    ];
    args.extend(
        current
            .iter()
            .filter(|&&(a, p)| (a, p) != (cfg.addr, cfg.prefix))
            .map(|(a, p)| format!("{a}/{p}")),
    );
    args
}

/// A card name as the kernel allows, and that `ip` can't mistake for an option.
fn plain_device_name(dev: &str) -> bool {
    !dev.is_empty()
        && !dev.starts_with('-')
        && dev
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-@".contains(&b))
}

/// Set `cfg` on `dev` as root, through sudo with `password`. Blocks until sudo
/// is done (a wrong password takes a few seconds), so call it off the event
/// loop. The password is wiped from memory before returning.
pub fn apply(dev: &str, cfg: &Ipv4Config, mut password: String) -> Result<(), ApplyError> {
    let result = run_sudo(dev, cfg, &password);
    wipe(&mut password);
    result
}

fn run_sudo(dev: &str, cfg: &Ipv4Config, password: &str) -> Result<(), ApplyError> {
    if !plain_device_name(dev) {
        return Err(ApplyError::Failed(format!("unexpected card name {dev:?}")));
    }
    let current = status(dev)
        .map(|s| s.addrs)
        .map_err(|e| ApplyError::Failed(describe(&e)))?;
    // -k: always ask, so the password line is always read by sudo itself.
    // -S: read it from stdin. -p "": no prompt text on stderr.
    let mut child = Command::new("/usr/bin/sudo")
        .args([
            "-k",
            "-S",
            "-p",
            "",
            "--",
            "/bin/sh",
            "-c",
            APPLY_SCRIPT,
            "simplewm-net",
        ])
        .args(script_args(dev, cfg, &current))
        // English messages, to tell a wrong password from other failures.
        .env("LC_ALL", "C")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ApplyError::Failed(format!("cannot run sudo: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A failed write means sudo already quit; its exit status says why.
        let mut line = Vec::with_capacity(password.len() + 1);
        line.extend_from_slice(password.as_bytes());
        line.push(b'\n');
        let _ = stdin.write_all(&line);
        wipe_bytes(&mut line);
    }
    let out = child
        .wait_with_output()
        .map_err(|e| ApplyError::Failed(format!("sudo failed: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    Err(sudo_error(&String::from_utf8_lossy(&out.stderr)))
}

/// Make sense of sudo's (or the script's) stderr after a failure.
fn sudo_error(stderr: &str) -> ApplyError {
    if stderr.contains("incorrect password") || stderr.contains("Sorry, try again") {
        return ApplyError::WrongPassword;
    }
    if stderr.contains("not in the sudoers file") || stderr.contains("not allowed to execute") {
        return ApplyError::Failed("your account is not allowed to use sudo.".into());
    }
    let last = stderr.lines().rev().find(|l| !l.trim().is_empty());
    let msg = last.unwrap_or("sudo failed").trim();
    ApplyError::Failed(match msg {
        "Error: Nexthop has invalid gateway." => {
            "the gateway is not reachable from that address.".into()
        }
        _ => msg.trim_start_matches("Error: ").to_string(),
    })
}

/// Overwrite a secret before its memory is freed.
pub fn wipe(s: &mut String) {
    let mut bytes = std::mem::take(s).into_bytes();
    wipe_bytes(&mut bytes);
}

fn wipe_bytes(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: a plain write to a byte we own; volatile so it isn't optimized away.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

/// A short explanation of a failed `status`.
pub fn describe(err: &io::Error) -> String {
    err.to_string()
}

// --- rtnetlink -------------------------------------------------------------------

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_DUMP: u16 = 0x300;

const RTM_GETADDR: u16 = 22;
const RTM_GETROUTE: u16 = 26;

const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;

const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;

const RTN_UNICAST: u8 = 1;

const HDR_LEN: usize = 16;

/// An IPv4 address on the card, as the kernel reports it.
struct Addr {
    local: Ipv4Addr,
    prefix: u8,
}

/// A default route through the card.
struct Route {
    gateway: Option<Ipv4Addr>,
}

/// A netlink message being built: header, fixed body, then attributes.
struct Msg(Vec<u8>);

impl Msg {
    fn new(ty: u16, flags: u16, body: &[u8]) -> Self {
        let mut v = vec![0; HDR_LEN];
        v[4..6].copy_from_slice(&ty.to_ne_bytes());
        v[6..8].copy_from_slice(&flags.to_ne_bytes());
        v.extend_from_slice(body);
        Msg(v)
    }
}

fn ifaddrmsg(prefix: u8, scope: u8, index: u32) -> Vec<u8> {
    let mut b = vec![libc::AF_INET as u8, prefix, 0, scope];
    b.extend_from_slice(&index.to_ne_bytes());
    b
}

fn rtmsg(table: u8, protocol: u8, scope: u8, ty: u8) -> Vec<u8> {
    // family, dst_len, src_len, tos, table, protocol, scope, type, flags (u32)
    let mut b = vec![libc::AF_INET as u8, 0, 0, 0, table, protocol, scope, ty];
    b.extend_from_slice(&0u32.to_ne_bytes());
    b
}

/// Attributes after a message's fixed body: (type, data).
fn attrs(mut b: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while b.len() >= 4 {
        let len = u16::from_ne_bytes([b[0], b[1]]) as usize;
        let ty = u16::from_ne_bytes([b[2], b[3]]) & 0x3fff;
        if len < 4 || len > b.len() {
            break;
        }
        out.push((ty, &b[4..len]));
        b = &b[len.next_multiple_of(4).min(b.len())..];
    }
    out
}

fn ipv4(data: &[u8]) -> Option<Ipv4Addr> {
    <[u8; 4]>::try_from(data).ok().map(Ipv4Addr::from)
}

fn u32_of(data: &[u8]) -> Option<u32> {
    <[u8; 4]>::try_from(data).ok().map(u32::from_ne_bytes)
}

struct Netlink {
    fd: OwnedFd,
    seq: u32,
}

impl Netlink {
    fn open() -> io::Result<Self> {
        // SAFETY: plain socket(2)/bind(2) calls; the fd is owned right away.
        unsafe {
            let fd = libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut sa: libc::sockaddr_nl = std::mem::zeroed();
            sa.nl_family = libc::AF_NETLINK as u16;
            let len = std::mem::size_of::<libc::sockaddr_nl>() as u32;
            if libc::bind(
                fd.as_raw_fd(),
                &sa as *const _ as *const libc::sockaddr,
                len,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Netlink { fd, seq: 0 })
        }
    }

    /// Send `msg` and collect the replies: each message's bytes after the
    /// netlink header. Ends at the ack (or error) or, for dumps, at DONE.
    fn request(&mut self, mut msg: Msg) -> io::Result<Vec<Vec<u8>>> {
        self.seq += 1;
        let seq = self.seq;
        let len = msg.0.len() as u32;
        msg.0[0..4].copy_from_slice(&len.to_ne_bytes());
        msg.0[8..12].copy_from_slice(&seq.to_ne_bytes());

        // SAFETY: the buffers outlive the calls; the kernel is the destination.
        unsafe {
            let mut sa: libc::sockaddr_nl = std::mem::zeroed();
            sa.nl_family = libc::AF_NETLINK as u16;
            let sent = libc::sendto(
                self.fd.as_raw_fd(),
                msg.0.as_ptr().cast(),
                msg.0.len(),
                0,
                &sa as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            );
            if sent < 0 {
                return Err(io::Error::last_os_error());
            }
        }

        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            // SAFETY: recv into a buffer we own, at most its length.
            let n =
                unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            let mut b = &buf[..n as usize];
            while b.len() >= HDR_LEN {
                let len = u32::from_ne_bytes(b[0..4].try_into().unwrap()) as usize;
                let ty = u16::from_ne_bytes([b[4], b[5]]);
                let mseq = u32::from_ne_bytes(b[8..12].try_into().unwrap());
                if len < HDR_LEN || len > b.len() {
                    break;
                }
                let payload = &b[HDR_LEN..len];
                b = &b[len.next_multiple_of(4).min(b.len())..];
                if mseq != seq {
                    continue;
                }
                match ty {
                    NLMSG_DONE => return Ok(out),
                    NLMSG_ERROR => {
                        let code = payload
                            .get(0..4)
                            .map_or(0, |e| i32::from_ne_bytes(e.try_into().unwrap()));
                        return if code == 0 {
                            Ok(out)
                        } else {
                            Err(io::Error::from_raw_os_error(-code))
                        };
                    }
                    _ => out.push(payload.to_vec()),
                }
            }
        }
    }

    fn addresses(&mut self, index: u32) -> io::Result<Vec<Addr>> {
        let replies = self.request(Msg::new(
            RTM_GETADDR,
            NLM_F_REQUEST | NLM_F_DUMP,
            &ifaddrmsg(0, 0, 0),
        ))?;
        let mut out = Vec::new();
        for r in replies {
            if r.len() < 8
                || r[0] != libc::AF_INET as u8
                || u32::from_ne_bytes(r[4..8].try_into().unwrap()) != index
            {
                continue;
            }
            let (mut local, mut address) = (None, None);
            for (ty, data) in attrs(&r[8..]) {
                match ty {
                    IFA_LOCAL => local = ipv4(data),
                    IFA_ADDRESS => address = ipv4(data),
                    _ => {}
                }
            }
            if let Some(local) = local.or(address) {
                out.push(Addr {
                    local,
                    prefix: r[1],
                });
            }
        }
        Ok(out)
    }

    /// IPv4 default routes (any table) that leave through the card.
    fn default_routes(&mut self, index: u32) -> io::Result<Vec<Route>> {
        let body = rtmsg(0, 0, 0, 0);
        let replies = self.request(Msg::new(RTM_GETROUTE, NLM_F_REQUEST | NLM_F_DUMP, &body))?;
        let mut out = Vec::new();
        for r in replies {
            // rtmsg: family, dst_len, src_len, tos, table, protocol, scope, type, flags
            if r.len() < 12 || r[0] != libc::AF_INET as u8 || r[1] != 0 || r[7] != RTN_UNICAST {
                continue;
            }
            let mut route = Route { gateway: None };
            let (mut oif, mut has_dst) = (None, false);
            for (ty, data) in attrs(&r[12..]) {
                match ty {
                    RTA_OIF => oif = u32_of(data),
                    RTA_GATEWAY => route.gateway = ipv4(data),
                    RTA_DST => has_dst = true,
                    _ => {}
                }
            }
            if oif == Some(index) && !has_dst {
                out.push(route);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields() {
        let c = parse(" 192.168.1.50/24 ", "192.168.1.1", " 1.1.1.1 ").unwrap();
        assert_eq!(c.addr, Ipv4Addr::new(192, 168, 1, 50));
        assert_eq!(c.prefix, 24);
        assert_eq!(c.gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(c.dns, Some(Ipv4Addr::new(1, 1, 1, 1)));
        let c = parse("10.0.0.2/8", "", "").unwrap();
        assert_eq!((c.gateway, c.dns), (None, None));
        assert!(parse("192.168.1.50", "", "").is_err());
        assert!(parse("192.168.1.50/33", "", "").is_err());
        assert!(parse("192.168.1.0/24", "", "").is_err());
        assert!(parse("192.168.1.255/24", "", "").is_err());
        assert!(parse("192.168.1.50/24", "192.168.2.1", "").is_err());
        assert!(parse("192.168.1.50/24", "192.168.1.50", "").is_err());
        assert!(parse("10.0.0.1/32", "", "").is_ok());
        // The DNS server may be anywhere, but must be a usable address.
        assert!(parse("10.0.0.2/8", "", "9.9.9.9").is_ok());
        assert!(parse("10.0.0.2/8", "", "127.0.0.53").is_ok());
        assert!(parse("10.0.0.2/8", "", "dns.example").is_err());
        assert!(parse("10.0.0.2/8", "", "1.1.1.1; reboot").is_err());
        assert!(parse("10.0.0.2/8", "", "0.0.0.0").is_err());
        assert!(parse("10.0.0.2/8", "", "224.0.0.1").is_err());
        assert!(parse("10.0.0.2/8", "", "255.255.255.255").is_err());
    }

    #[test]
    fn writes_and_reads_resolv_conf() {
        let conf = resolv_conf(Ipv4Addr::new(1, 1, 1, 1));
        let lines: Vec<&str> = conf.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("# simplewm owns this file"));
        assert_eq!(lines[1], "nameserver 1.1.1.1");
        assert_eq!(parse_nameservers(&conf), ["1.1.1.1"]);
        assert_eq!(
            parse_nameservers(
                "# x\nsearch lan\nnameserver 10.0.0.1\n  nameserver  9.9.9.9 # y\nnameserver\n"
            ),
            ["10.0.0.1", "9.9.9.9"]
        );
    }

    #[test]
    fn builds_script_args() {
        let cfg = parse("10.0.0.5/24", "10.0.0.1", "1.1.1.1").unwrap();
        let current = [
            (Ipv4Addr::new(10, 0, 0, 5), 24),
            (Ipv4Addr::new(192, 168, 1, 2), 24),
        ];
        // The new address is kept, not removed and re-added; the others go.
        assert_eq!(
            script_args("enp3s0", &cfg, &current),
            [
                "enp3s0",
                "10.0.0.5/24",
                "10.0.0.1",
                &resolv_conf(Ipv4Addr::new(1, 1, 1, 1)),
                "192.168.1.2/24"
            ]
        );
        let cfg = parse("10.0.0.5/24", "", "").unwrap();
        assert_eq!(
            script_args("eth0", &cfg, &[]),
            ["eth0", "10.0.0.5/24", "", ""]
        );
    }

    #[test]
    fn checks_device_names() {
        assert!(plain_device_name("enp3s0"));
        assert!(plain_device_name("eth0.100"));
        assert!(!plain_device_name(""));
        assert!(!plain_device_name("-x"));
        assert!(!plain_device_name("a b"));
        assert!(!plain_device_name("eth0;reboot"));
    }

    #[test]
    fn reads_sudo_errors() {
        let wrong = "Sorry, try again.\nsudo: no password was provided\nsudo: 1 incorrect password attempt\n";
        assert_eq!(sudo_error(wrong), ApplyError::WrongPassword);
        assert_eq!(
            sudo_error("james is not in the sudoers file.\n"),
            ApplyError::Failed("your account is not allowed to use sudo.".into())
        );
        assert_eq!(
            sudo_error("Error: Nexthop has invalid gateway.\n"),
            ApplyError::Failed("the gateway is not reachable from that address.".into())
        );
        assert_eq!(
            sudo_error("Error: Cannot find device \"d9\".\n\n"),
            ApplyError::Failed("Cannot find device \"d9\".".into())
        );
    }

    #[test]
    fn wipes_secrets() {
        let mut s = String::from("hunter2");
        wipe(&mut s);
        assert!(s.is_empty());
        let mut b = *b"secret";
        wipe_bytes(&mut b);
        assert_eq!(b, [0; 6]);
    }

    #[test]
    fn reads_the_card() {
        // Read-only netlink dumps work without any capability.
        if let Some(dev) = ethernet_device() {
            status(&dev).unwrap();
        }
    }
}

#[cfg(test)]
mod namespace_test {
    use std::process::Command;

    use super::*;

    /// Run `APPLY_SCRIPT` the way `apply` does, minus sudo (the test is
    /// already root in its namespace), writing `resolv` instead of
    /// /etc/resolv.conf.
    fn run_script(dev: &str, cfg: &Ipv4Config, resolv: &Path) -> Result<(), ApplyError> {
        let current = status(dev).unwrap().addrs;
        let script = APPLY_SCRIPT.replacen(
            &format!("resolv={RESOLV_CONF}\n"),
            &format!("resolv={}\n", resolv.display()),
            1,
        );
        assert_ne!(script, APPLY_SCRIPT);
        let out = Command::new("/bin/sh")
            .args(["-c", &script, "simplewm-net"])
            .args(script_args(dev, cfg, &current))
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        if out.status.success() {
            Ok(())
        } else {
            Err(sudo_error(&String::from_utf8_lossy(&out.stderr)))
        }
    }

    /// Run inside a scratch network namespace with a dummy card:
    /// `unshare -rnm sh -c 'mount -t sysfs none /sys && ip link add d0 type dummy && SIMPLEWM_TEST_DEV=d0 cargo test applies_to_a_card -- --ignored'`.
    #[test]
    #[ignore]
    fn applies_to_a_card() {
        let dev = std::env::var("SIMPLEWM_TEST_DEV").expect("SIMPLEWM_TEST_DEV");
        let dir = std::env::temp_dir().join(format!("simplewm-ns-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let resolv = dir.join("resolv.conf");
        // A symlink, like systemd-resolved leaves, is replaced by the file.
        std::os::unix::fs::symlink("/nonexistent/stub-resolv.conf", &resolv).unwrap();

        let first = parse("10.1.2.3/24", "10.1.2.1", "1.1.1.1").unwrap();
        run_script(&dev, &first, &resolv).unwrap();
        let s = status(&dev).unwrap();
        assert!(s.up);
        assert_eq!(s.addrs, [(first.addr, 24)]);
        assert_eq!(s.gateways, [first.gateway.unwrap()]);
        assert!(!fs::symlink_metadata(&resolv).unwrap().is_symlink());
        assert_eq!(
            fs::read_to_string(&resolv).unwrap(),
            resolv_conf(first.dns.unwrap())
        );
        // Applying the same again is fine.
        run_script(&dev, &first, &resolv).unwrap();
        assert_eq!(status(&dev).unwrap().addrs, [(first.addr, 24)]);
        // Applying something else replaces all three.
        let second = parse("172.16.0.9/16", "172.16.0.1", "9.9.9.9").unwrap();
        run_script(&dev, &second, &resolv).unwrap();
        let s = status(&dev).unwrap();
        assert_eq!(s.addrs, [(second.addr, 16)]);
        assert_eq!(s.gateways, [second.gateway.unwrap()]);
        assert_eq!(
            fs::read_to_string(&resolv).unwrap(),
            resolv_conf(second.dns.unwrap())
        );
        // No gateway: the default route goes away. No DNS server: the file stays.
        let third = parse("172.16.0.9/16", "", "").unwrap();
        run_script(&dev, &third, &resolv).unwrap();
        assert!(status(&dev).unwrap().gateways.is_empty());
        assert_eq!(
            fs::read_to_string(&resolv).unwrap(),
            resolv_conf(second.dns.unwrap())
        );
        // Nothing is left beside it.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }
}
