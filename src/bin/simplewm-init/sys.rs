//! Thin wrappers around the system calls an init needs.

use std::{
    ffi::CString,
    io,
    os::{fd::RawFd, unix::fs::MetadataExt},
    path::Path,
};

fn cstr(s: &str) -> CString {
    CString::new(s).expect("no NUL in a fixed string")
}

fn check(r: libc::c_int) -> io::Result<()> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Mount `fstype` on `target` (made if missing).
pub fn mount(source: &str, target: &str, fstype: &str, flags: u64, data: &str) -> io::Result<()> {
    std::fs::create_dir_all(target)?;
    let (source, target, fstype, data) = (cstr(source), cstr(target), cstr(fstype), cstr(data));
    // SAFETY: all pointers are valid NUL-terminated strings for the call.
    check(unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags as libc::c_ulong,
            data.as_ptr().cast(),
        )
    })
}

/// Unmount the filesystem on `target` (at shutdown).
pub fn unmount(target: &str) -> io::Result<()> {
    let target = cstr(target);
    // SAFETY: a valid NUL-terminated string.
    check(unsafe { libc::umount(target.as_ptr()) })
}

/// Remount the root filesystem read-only (at shutdown).
pub fn remount_root_read_only() -> io::Result<()> {
    let root = cstr("/");
    // SAFETY: valid strings; null fstype and data are allowed for a remount.
    check(unsafe {
        libc::mount(
            std::ptr::null(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT | libc::MS_RDONLY,
            std::ptr::null(),
        )
    })
}

/// Whether `path` is a mount point (on a different filesystem from its parent).
pub fn is_mount_point(path: &str) -> bool {
    let parent = Path::new(path).parent().unwrap_or(Path::new("/"));
    match (std::fs::metadata(path), std::fs::metadata(parent)) {
        (Ok(p), Ok(parent)) => p.dev() != parent.dev(),
        _ => false,
    }
}

pub fn set_hostname(name: &str) -> io::Result<()> {
    // SAFETY: the pointer and length describe `name`.
    check(unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) })
}

/// Bring up the loopback interface `lo`.
pub fn loopback_up() -> io::Result<()> {
    // SAFETY: plain socket creation.
    let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    check(sock)?;
    // SAFETY: ifreq is plain data; all zeroes is a valid starting value.
    let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
    for (dst, src) in req.ifr_name.iter_mut().zip(b"lo") {
        *dst = *src as libc::c_char;
    }
    // SAFETY: `req` is a valid ifreq for both ioctls; ifru_flags is the field they use.
    let result = unsafe {
        check(libc::ioctl(sock, libc::SIOCGIFFLAGS as _, &mut req)).and_then(|()| {
            req.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
            check(libc::ioctl(sock, libc::SIOCSIFFLAGS as _, &req))
        })
    };
    // SAFETY: closing the socket we opened.
    unsafe { libc::close(sock) };
    result
}

/// Make Ctrl+Alt+Del send PID 1 SIGINT instead of rebooting at once.
pub fn disable_ctrl_alt_del() {
    // SAFETY: a plain reboot(2) call that only changes a setting.
    unsafe { libc::reboot(libc::RB_DISABLE_CAD) };
}

/// Power off, reboot or halt (`RB_*`); only returns if it failed.
pub fn reboot(how: libc::c_int) -> io::Error {
    // SAFETY: plain reboot(2) call.
    unsafe { libc::reboot(how) };
    io::Error::last_os_error()
}

pub fn sync() {
    // SAFETY: no arguments.
    unsafe { libc::sync() };
}

/// Send `signal` to every process but PID 1.
pub fn kill_all(signal: libc::c_int) {
    // SAFETY: plain kill(2) call.
    unsafe { libc::kill(-1, signal) };
}

/// SIGKILL the process group led by `pid` (a shell and what it runs).
pub fn kill_session(pid: i32) {
    // SAFETY: plain kill(2) call on a process group.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
}

/// The signals PID 1 handles, blocked and read from a signalfd instead.
pub struct Signals {
    fd: RawFd,
}

impl Signals {
    pub fn new(signals: &[libc::c_int]) -> io::Result<Signals> {
        // SAFETY: sigset operations on a local, then blocking it for this (only) thread.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            for &s in signals {
                libc::sigaddset(&mut set, s);
            }
            check(libc::sigprocmask(
                libc::SIG_BLOCK,
                &set,
                std::ptr::null_mut(),
            ))?;
            let fd = libc::signalfd(-1, &set, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK);
            check(fd)?;
            Ok(Signals { fd })
        }
    }

    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// The next pending signal, if any.
    pub fn next(&self) -> Option<libc::c_int> {
        // SAFETY: signalfd_siginfo is plain data, and read writes at most its size.
        unsafe {
            let mut info: libc::signalfd_siginfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::signalfd_siginfo>();
            let n = libc::read(self.fd, (&raw mut info).cast(), size);
            (n == size as isize).then_some(info.ssi_signo as libc::c_int)
        }
    }
}

/// Reap one exited child: its PID and wait status.
pub fn reap() -> Option<(i32, i32)> {
    let mut status = 0;
    // SAFETY: plain waitpid(2) call.
    let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    (pid > 0).then_some((pid, status))
}

/// Wait until one of `fds` is readable or `timeout_ms` passes (-1: no limit).
/// Returns which were readable.
pub fn poll(fds: &[RawFd], timeout_ms: i32) -> Vec<bool> {
    let mut pfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    // SAFETY: `pfds` is a valid array of its length.
    let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, timeout_ms) };
    pfds.iter().map(|p| n > 0 && p.revents != 0).collect()
}

/// How a child ended, in words.
pub fn describe_status(status: i32) -> String {
    if libc::WIFEXITED(status) {
        format!("exited with status {}", libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        let sig = libc::WTERMSIG(status);
        // SAFETY: strsignal returns a static (or thread-local) NUL-terminated string.
        let name = unsafe { std::ffi::CStr::from_ptr(libc::strsignal(sig)) };
        format!("killed by signal {sig} ({})", name.to_string_lossy())
    } else {
        format!("ended (status {status:#x})")
    }
}

/// Whether any child is still alive. Reaps one that has exited (at shutdown, where its
/// status doesn't matter).
pub fn has_children() -> bool {
    let mut status = 0;
    // SAFETY: plain waitpid(2) call. 0: children, none exited; -1 (ECHILD): none at all.
    let r = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    r >= 0
}

/// The processes other than PID 1 and kernel threads, as "name (pid)".
pub fn other_processes() -> Vec<String> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut found: Vec<(u32, String)> = dir
        .flatten()
        .filter_map(|e| {
            let pid: u32 = e.file_name().to_str()?.parse().ok()?;
            // Kernel threads have no command line.
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            if pid == 1 || cmdline.is_empty() {
                return None;
            }
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
            Some((pid, format!("{} ({pid})", comm.trim())))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, s)| s).collect()
}
