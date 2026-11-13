//! Static IPv4 setup for the wired network card, behind the Network applet.
//!
//! simplewm sets the address, the default route and the link state itself,
//! over rtnetlink. That needs CAP_NET_ADMIN, which `make` gives the binary with
//! `setcap` (the capability is not passed on to programs simplewm launches).
//! Card details are read from /sys/class/net.

use std::{
    fs, io,
    net::Ipv4Addr,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
};

/// Bit of CAP_NET_ADMIN in the capability sets.
const CAP_NET_ADMIN: u32 = 12;

/// An address with its prefix length, and the gateway for the default route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Config {
    pub addr: Ipv4Addr,
    pub prefix: u8,
    pub gateway: Option<Ipv4Addr>,
}

/// Parse the applet's fields: "192.168.1.50/24" and "192.168.1.1" (the gateway
/// may be empty for no default route).
pub fn parse(address: &str, gateway: &str) -> Result<Ipv4Config, String> {
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
    Ok(Ipv4Config {
        addr,
        prefix,
        gateway,
    })
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

/// Whether this process can change network settings.
pub fn has_net_admin() -> bool {
    let Ok(s) = fs::read_to_string("/proc/self/status") else {
        return false;
    };
    s.lines()
        .find_map(|l| l.strip_prefix("CapEff:"))
        .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
        .is_some_and(|caps| caps & (1 << CAP_NET_ADMIN) != 0)
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

/// Switch the card on, replace its IPv4 addresses with `cfg.addr`, and point
/// the default route at `cfg.gateway`.
pub fn apply(dev: &str, cfg: &Ipv4Config) -> io::Result<()> {
    let index = ifindex(dev)?;
    let mut nl = Netlink::open()?;
    nl.set_up(index)?;
    for route in nl.default_routes(index)? {
        ignore_missing(nl.del_route(index, &route))?;
    }
    for addr in nl.addresses(index)? {
        if (addr.local, addr.prefix) != (cfg.addr, cfg.prefix) {
            ignore_missing(nl.del_addr(index, &addr))?;
        }
    }
    nl.add_addr(index, cfg.addr, cfg.prefix)?;
    if let Some(gw) = cfg.gateway {
        nl.add_default_route(index, gw)?;
    }
    Ok(())
}

/// Deleting something that went away meanwhile (removing an address also
/// removes the routes through it) is fine.
fn ignore_missing(r: io::Result<()>) -> io::Result<()> {
    match r {
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::ESRCH | libc::ENOENT | libc::EADDRNOTAVAIL)
            ) =>
        {
            Ok(())
        }
        r => r,
    }
}

/// A short explanation of a failed `apply`/`status`.
pub fn describe(err: &io::Error) -> String {
    match err.raw_os_error() {
        Some(libc::EPERM) => {
            "Not allowed: simplewm is missing CAP_NET_ADMIN (run make, which runs setcap).".into()
        }
        Some(libc::ENETUNREACH) => "The gateway is not reachable from that address.".into(),
        _ => err.to_string(),
    }
}

// --- rtnetlink -------------------------------------------------------------------

const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_REPLACE: u16 = 0x100;
const NLM_F_DUMP: u16 = 0x300;
const NLM_F_CREATE: u16 = 0x400;

const RTM_NEWLINK: u16 = 16;
const RTM_NEWADDR: u16 = 20;
const RTM_DELADDR: u16 = 21;
const RTM_GETADDR: u16 = 22;
const RTM_NEWROUTE: u16 = 24;
const RTM_DELROUTE: u16 = 25;
const RTM_GETROUTE: u16 = 26;

const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_BROADCAST: u16 = 4;

const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PRIORITY: u16 = 6;
const RTA_TABLE: u16 = 15;

const RT_TABLE_MAIN: u8 = 254;
const RTPROT_STATIC: u8 = 4;
const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_SCOPE_NOWHERE: u8 = 255;
const RTN_UNICAST: u8 = 1;

const HDR_LEN: usize = 16;

/// An IPv4 address on the card, as the kernel reports it.
struct Addr {
    local: Ipv4Addr,
    /// The peer address (the same as `local` unless point-to-point).
    address: Ipv4Addr,
    prefix: u8,
}

/// A default route through the card.
struct Route {
    gateway: Option<Ipv4Addr>,
    priority: Option<u32>,
    table: u32,
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

    fn attr(mut self, ty: u16, data: &[u8]) -> Self {
        let len = 4 + data.len();
        self.0.extend_from_slice(&(len as u16).to_ne_bytes());
        self.0.extend_from_slice(&ty.to_ne_bytes());
        self.0.extend_from_slice(data);
        self.0.resize(self.0.len().next_multiple_of(4), 0);
        self
    }

    fn ip(self, ty: u16, ip: Ipv4Addr) -> Self {
        self.attr(ty, &ip.octets())
    }

    fn u32(self, ty: u16, v: u32) -> Self {
        self.attr(ty, &v.to_ne_bytes())
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

    fn ack(&mut self, msg: Msg) -> io::Result<()> {
        self.request(msg).map(drop)
    }

    fn set_up(&mut self, index: u32) -> io::Result<()> {
        // ifinfomsg: family, pad, type (u16), index (i32), flags, change
        let mut body = vec![libc::AF_UNSPEC as u8, 0, 0, 0];
        body.extend_from_slice(&index.to_ne_bytes());
        body.extend_from_slice(&(libc::IFF_UP as u32).to_ne_bytes());
        body.extend_from_slice(&(libc::IFF_UP as u32).to_ne_bytes());
        self.ack(Msg::new(RTM_NEWLINK, NLM_F_REQUEST | NLM_F_ACK, &body))
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
                    address: address.unwrap_or(local),
                    prefix: r[1],
                });
            }
        }
        Ok(out)
    }

    fn del_addr(&mut self, index: u32, a: &Addr) -> io::Result<()> {
        let msg = Msg::new(
            RTM_DELADDR,
            NLM_F_REQUEST | NLM_F_ACK,
            &ifaddrmsg(a.prefix, 0, index),
        )
        .ip(IFA_LOCAL, a.local)
        .ip(IFA_ADDRESS, a.address);
        self.ack(msg)
    }

    fn add_addr(&mut self, index: u32, addr: Ipv4Addr, prefix: u8) -> io::Result<()> {
        let flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE;
        let mut msg = Msg::new(
            RTM_NEWADDR,
            flags,
            &ifaddrmsg(prefix, RT_SCOPE_UNIVERSE, index),
        )
        .ip(IFA_LOCAL, addr)
        .ip(IFA_ADDRESS, addr);
        if prefix <= 30 {
            msg = msg.ip(
                IFA_BROADCAST,
                Ipv4Addr::from(u32::from(addr) | !mask(prefix)),
            );
        }
        self.ack(msg)
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
            let mut route = Route {
                gateway: None,
                priority: None,
                table: r[4] as u32,
            };
            let (mut oif, mut has_dst) = (None, false);
            for (ty, data) in attrs(&r[12..]) {
                match ty {
                    RTA_OIF => oif = u32_of(data),
                    RTA_GATEWAY => route.gateway = ipv4(data),
                    RTA_PRIORITY => route.priority = u32_of(data),
                    RTA_TABLE => route.table = u32_of(data).unwrap_or(route.table),
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

    fn del_route(&mut self, index: u32, route: &Route) -> io::Result<()> {
        let table = if route.table < 256 {
            route.table as u8
        } else {
            0
        };
        let mut msg = Msg::new(
            RTM_DELROUTE,
            NLM_F_REQUEST | NLM_F_ACK,
            &rtmsg(table, 0, RT_SCOPE_NOWHERE, 0),
        )
        .u32(RTA_TABLE, route.table)
        .u32(RTA_OIF, index);
        if let Some(gw) = route.gateway {
            msg = msg.ip(RTA_GATEWAY, gw);
        }
        if let Some(p) = route.priority {
            msg = msg.u32(RTA_PRIORITY, p);
        }
        self.ack(msg)
    }

    fn add_default_route(&mut self, index: u32, gateway: Ipv4Addr) -> io::Result<()> {
        let flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE;
        let body = rtmsg(RT_TABLE_MAIN, RTPROT_STATIC, RT_SCOPE_UNIVERSE, RTN_UNICAST);
        let msg = Msg::new(RTM_NEWROUTE, flags, &body)
            .ip(RTA_GATEWAY, gateway)
            .u32(RTA_OIF, index);
        self.ack(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields() {
        let c = parse(" 192.168.1.50/24 ", "192.168.1.1").unwrap();
        assert_eq!(c.addr, Ipv4Addr::new(192, 168, 1, 50));
        assert_eq!(c.prefix, 24);
        assert_eq!(c.gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(parse("10.0.0.2/8", "").unwrap().gateway, None);
        assert!(parse("192.168.1.50", "").is_err());
        assert!(parse("192.168.1.50/33", "").is_err());
        assert!(parse("192.168.1.0/24", "").is_err());
        assert!(parse("192.168.1.255/24", "").is_err());
        assert!(parse("192.168.1.50/24", "192.168.2.1").is_err());
        assert!(parse("192.168.1.50/24", "192.168.1.50").is_err());
        assert!(parse("10.0.0.1/32", "").is_ok());
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
    /// Run inside a scratch network namespace with a dummy card, e.g.
    /// `unshare -rn sh -c 'ip link add d0 type dummy; SIMPLEWM_TEST_DEV=d0 <test binary> --ignored'`.
    #[test]
    #[ignore]
    fn applies_to_a_card() {
        let dev = std::env::var("SIMPLEWM_TEST_DEV").expect("SIMPLEWM_TEST_DEV");
        let first = super::parse("10.1.2.3/24", "10.1.2.1").unwrap();
        super::apply(&dev, &first).unwrap();
        let s = super::status(&dev).unwrap();
        assert!(s.up);
        assert_eq!(s.addrs, [(first.addr, 24)]);
        assert_eq!(s.gateways, [first.gateway.unwrap()]);
        // Applying again replaces both.
        let second = super::parse("172.16.0.9/16", "172.16.0.1").unwrap();
        super::apply(&dev, &second).unwrap();
        let s = super::status(&dev).unwrap();
        assert_eq!(s.addrs, [(second.addr, 16)]);
        assert_eq!(s.gateways, [second.gateway.unwrap()]);
        // No gateway: the default route goes away.
        let third = super::parse("172.16.0.9/16", "").unwrap();
        super::apply(&dev, &third).unwrap();
        assert!(super::status(&dev).unwrap().gateways.is_empty());
    }
}
