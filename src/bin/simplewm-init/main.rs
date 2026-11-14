//! simplewm-init: PID 1 for the simplewm distro (see `distro/CLAUDE.md`).
//!
//! It mounts the basic filesystems, starts udevd (devices) and seatd (screen and input
//! access) as root, then runs simplewm as the user (the account with UID 1000) and keeps
//! it running. There is nothing to configure.
//!
//! The screen has one VT, tty1, and only simplewm runs there. When simplewm exits, the
//! kernel's text console comes back on tty1 and simplewm-init writes why there (and on
//! the serial console, when the kernel uses one: QEMU's terminal). Whatever the reason,
//! even quitting on purpose, it doesn't restart simplewm: it starts a login shell as the
//! user on tty1, with exactly the environment simplewm gets, so running `simplewm` there
//! shows what goes wrong. When that shell exits, simplewm is started again.
//!
//! Shutting down: busybox's `poweroff`, `reboot` and `halt` signal PID 1 with SIGUSR2,
//! SIGTERM and SIGUSR1. Everything gets SIGTERM, then SIGKILL after 5 s, the root
//! filesystem is remounted read-only, and the machine powers off, reboots or halts.
//! Ctrl+Alt+Del does nothing (no keyboard shortcuts).

mod account;
mod home;
mod sys;

use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use account::{Account, USER_UID};

const PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// The screen's one VT.
const SCREEN_TTY: &str = "/dev/tty1";
/// Lines of simplewm's log shown when it exits.
const LOG_TAIL: usize = 20;
/// When even the shell can't start, how long until simplewm is tried again.
const NO_SHELL_RETRY: Duration = Duration::from_secs(60);
const STOP_GRACE: Duration = Duration::from_secs(5);
/// The screen's console font (Terminus 16x32, from font-terminus), loaded with setfont.
const CONSOLE_FONT: &str = "/usr/share/consolefonts/ter-132n.psf.gz";

fn main() {
    if std::process::id() != 1 {
        eprintln!("simplewm-init is the system's first program (PID 1); the kernel starts it.");
        std::process::exit(1);
    }
    // The kernel starts PID 1 with no PATH.
    // SAFETY: nothing else is running yet (one thread, no children).
    unsafe { std::env::set_var("PATH", PATH) };

    let result = std::panic::catch_unwind(run);
    // PID 1 must never exit: the kernel would panic. Keep reaping orphans.
    let why = match result {
        Ok(Err(e)) => e,
        Ok(Ok(())) => "stopped".into(),
        Err(_) => "crashed".into(),
    };
    say_now(&format!(
        "{why}; nothing more will start. Turn the machine off."
    ));
    loop {
        while sys::reap().is_some() {}
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Write to the console before `Console` exists (or after it's gone).
fn say_now(msg: &str) {
    eprintln!("simplewm-init: {msg}");
}

fn run() -> Result<(), String> {
    let signals = sys::Signals::new(&[
        libc::SIGCHLD,
        libc::SIGTERM,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGINT,
    ])
    .map_err(|e| format!("can't set up signals: {e}"))?;
    sys::disable_ctrl_alt_del();

    mount_basics();
    let console = Console::new();
    console.say("starting");
    match home::mount() {
        home::Outcome::NoPartition => {}
        home::Outcome::Mounted(dev) => {
            console.say(&format!("home: {} on {}", dev.display(), home::MOUNT_POINT));
        }
        home::Outcome::Failed(dev, e) => console.say(&format!(
            "can't mount the home partition {} on {}: {e}; using the system's {} instead",
            dev.display(),
            home::MOUNT_POINT,
            home::MOUNT_POINT
        )),
    }
    if let Ok(name) = std::fs::read_to_string("/etc/hostname")
        && let Err(e) = sys::set_hostname(name.trim())
    {
        console.say(&format!("can't set the hostname: {e}"));
    }
    if let Err(e) = sys::loopback_up() {
        console.say(&format!("can't bring up lo: {e}"));
    }
    let _ = std::fs::create_dir_all("/var/log");

    start_udev(&console);
    // The kernel's own console font is 8x16, tiny on a big screen. Only once udev has
    // loaded the GPU's driver: before that the console may be VGA text mode, which only
    // takes fonts 8 or 9 pixels wide.
    run_and_wait(&console, "setfont", &["-C", SCREEN_TTY, CONSOLE_FONT]);
    start_seatd(&console);

    let user = Account::load(USER_UID)?;
    let runtime =
        make_runtime_dir(&user).map_err(|e| format!("can't make the runtime dir: {e}"))?;

    // udevadm settle should have waited for the GPU's driver; allow a moment more anyway.
    for _ in 0..50 {
        if Path::new("/dev/dri/card0").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !Path::new("/dev/dri/card0").exists() {
        console.say("no GPU (/dev/dri/card0) yet; starting simplewm anyway");
    }

    let mut sup = Supervisor {
        console,
        user,
        runtime,
        simplewm: None,
        start_at: Some(Instant::now()),
        shell: None,
    };
    sup.run(&signals);
    Ok(())
}

fn mount_basics() {
    let nosuid_nodev = libc::MS_NOSUID | libc::MS_NODEV;
    let mounts: [(&str, &str, &str, u64, &str); 7] = [
        ("proc", "/proc", "proc", nosuid_nodev | libc::MS_NOEXEC, ""),
        ("sys", "/sys", "sysfs", nosuid_nodev | libc::MS_NOEXEC, ""),
        ("dev", "/dev", "devtmpfs", libc::MS_NOSUID, "mode=755"),
        (
            "devpts",
            "/dev/pts",
            "devpts",
            libc::MS_NOSUID | libc::MS_NOEXEC,
            "gid=5,mode=620",
        ),
        ("shm", "/dev/shm", "tmpfs", nosuid_nodev, "mode=1777"),
        ("run", "/run", "tmpfs", nosuid_nodev, "mode=755"),
        ("tmp", "/tmp", "tmpfs", nosuid_nodev, "mode=1777"),
    ];
    for (source, target, fstype, flags, data) in mounts {
        // The initramfs already moved its /dev here.
        if sys::is_mount_point(target) {
            continue;
        }
        if let Err(e) = sys::mount(source, target, fstype, flags, data) {
            say_now(&format!("can't mount {target}: {e}"));
        }
    }
    // The firmware's variables, for efibootmgr (only when the machine started through EFI;
    // not in the VM, which boots the kernel directly).
    let efivars = "/sys/firmware/efi/efivars";
    if std::path::Path::new(efivars).is_dir() && !sys::is_mount_point(efivars) {
        let flags = nosuid_nodev | libc::MS_NOEXEC;
        if let Err(e) = sys::mount("efivarfs", efivars, "efivarfs", flags, "") {
            say_now(&format!("can't mount {efivars}: {e}"));
        }
    }
}

/// udevd follows its rules: it loads drivers, gives /dev files their groups, marks sound
/// cards ready for PipeWire, and tells programs about devices that come and go.
fn start_udev(console: &Console) {
    run_and_wait(console, "udevd", &["--daemon"]);
    // Replay the devices that were there before udevd started, so their drivers load,
    // and wait until it has handled them all.
    run_and_wait(
        console,
        "udevadm",
        &["trigger", "--action=add", "--type=subsystems"],
    );
    run_and_wait(
        console,
        "udevadm",
        &["trigger", "--action=add", "--type=devices"],
    );
    run_and_wait(console, "udevadm", &["settle", "--timeout=10"]);
}

fn run_and_wait(console: &Console, program: &str, args: &[&str]) {
    match command(program).args(args).status() {
        Ok(s) if s.success() => {}
        Ok(s) => console.say(&format!("{program} {}: {s}", args.join(" "))),
        Err(e) => console.say(&format!("can't run {program}: {e}")),
    }
}

/// seatd opens the GPU and input devices for members of the seat group. It writes a line
/// to fd 3 when it's ready.
fn start_seatd(console: &Console) {
    let (mut ready_r, ready_w) = match io::pipe() {
        Ok(p) => p,
        Err(e) => return console.say(&format!("can't start seatd: {e}")),
    };
    let w = ready_w.as_raw_fd();
    let mut cmd = command("seatd");
    cmd.args(["-g", "seat", "-n", "3"]);
    if let Some((out, err)) = log_file(Path::new("/var/log/seatd.log"), None) {
        cmd.stdout(out).stderr(err);
    }
    // SAFETY: dup2 and fcntl are async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            if w == 3 {
                libc::fcntl(3, libc::F_SETFD, 0);
            } else if libc::dup2(w, 3) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if let Err(e) = cmd.spawn() {
        return console.say(&format!("can't start seatd: {e}"));
    }
    drop(ready_w);
    if !sys::poll(&[ready_r.as_raw_fd()], 5000)[0] {
        return console.say("seatd isn't ready after 5 s; going on");
    }
    let mut buf = [0u8; 16];
    if ready_r.read(&mut buf).unwrap_or(0) == 0 {
        console.say("seatd exited before it was ready (see /var/log/seatd.log)");
    }
}

/// A command for `program` that starts with no signals blocked. simplewm-init blocks
/// the signals it reads from a signalfd, and children would inherit that: simplewm would
/// then ignore SIGTERM, and so would everything it starts.
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    // SAFETY: sigemptyset and sigprocmask are async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            let mut set: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigprocmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
            Ok(())
        });
    }
    cmd
}

/// Open `path` for appending as a child's stdout and stderr, owned by `owner` if given.
fn log_file(path: &Path, owner: Option<&Account>) -> Option<(File, File)> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(path)
        .ok()?;
    if let Some(a) = owner {
        let _ = std::os::unix::fs::fchown(&file, Some(a.uid), Some(a.gid));
    }
    let err = file.try_clone().ok()?;
    Some((file, err))
}

/// `/run/user/<uid>`, where the Wayland, D-Bus and PipeWire sockets go.
fn make_runtime_dir(user: &Account) -> io::Result<PathBuf> {
    let dir = PathBuf::from(format!("/run/user/{}", user.uid));
    std::fs::create_dir_all(&dir)?;
    std::os::unix::fs::chown(&dir, Some(user.uid), Some(user.gid))?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Where simplewm-init's messages go: the screen's VT, and the serial console too when
/// the kernel was told to use one (`console=ttyS…`, as in QEMU).
struct Console {
    serial: bool,
}

impl Console {
    fn new() -> Console {
        let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
        Console {
            serial: cmdline
                .split_whitespace()
                .any(|a| a.starts_with("console=ttyS")),
        }
    }

    fn say(&self, msg: &str) {
        self.write(&format!("simplewm-init: {msg}\n"));
    }

    fn write(&self, text: &str) {
        // The kernel's text console wants \r\n.
        let text = text.replace('\n', "\r\n");
        if let Ok(mut tty) = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(SCREEN_TTY)
        {
            let _ = tty.write_all(text.as_bytes());
        }
        if self.serial {
            let _ = io::stderr().write_all(text.as_bytes());
        }
    }
}

struct Supervisor {
    console: Console,
    user: Account,
    runtime: PathBuf,
    /// simplewm's PID while it runs.
    simplewm: Option<i32>,
    /// When to start simplewm (again).
    start_at: Option<Instant>,
    /// The user's shell on tty1, after simplewm kept failing.
    shell: Option<i32>,
}

impl Supervisor {
    fn run(&mut self, signals: &sys::Signals) {
        loop {
            if self.start_at.is_some_and(|t| t <= Instant::now()) {
                self.start_at = None;
                self.start_simplewm();
            }
            let timeout = self
                .start_at
                .map(|t| t.saturating_duration_since(Instant::now()).as_millis() as i32)
                .unwrap_or(-1);
            sys::poll(&[signals.fd()], timeout);
            while let Some(signal) = signals.next() {
                match signal {
                    libc::SIGCHLD => self.reap(),
                    libc::SIGUSR2 => self.shutdown(signals, libc::RB_POWER_OFF, "powering off"),
                    libc::SIGTERM => self.shutdown(signals, libc::RB_AUTOBOOT, "rebooting"),
                    libc::SIGUSR1 => self.shutdown(signals, libc::RB_HALT_SYSTEM, "halting"),
                    _ => {} // SIGINT: Ctrl+Alt+Del, which does nothing.
                }
            }
        }
    }

    fn start_simplewm(&mut self) {
        self.console.say(&format!(
            "starting simplewm as {} (log: ~/simplewm.log)",
            self.user.name
        ));
        match spawn_simplewm(&self.user, &self.runtime) {
            Ok(pid) => self.simplewm = Some(pid),
            Err(e) => self.simplewm_ended(&format!("couldn't be started ({e})")),
        }
    }

    fn reap(&mut self) {
        while let Some((pid, status)) = sys::reap() {
            if self.simplewm == Some(pid) {
                self.simplewm = None;
                self.simplewm_ended(&sys::describe_status(status));
            } else if self.shell == Some(pid) {
                self.shell = None;
                let _ = std::os::unix::fs::chown(SCREEN_TTY, Some(0), None);
                self.console.say("the shell exited");
                self.start_at = Some(Instant::now());
            }
            // Anything else is an orphan that ended up with us; reaping it is enough.
        }
    }

    /// simplewm is gone (`how` follows "simplewm": "exited with status 1"). Never
    /// restart it here: give the user a shell on tty1 instead.
    fn simplewm_ended(&mut self, how: &str) {
        // Clear the screen so the report starts at the top: how it ended, the end of its
        // log, and what to do, just above the shell's prompt.
        self.console.write("\x1b[H\x1b[2J");
        self.console.say(&format!("simplewm {how}."));
        let log = std::fs::read(self.user.home.join("simplewm.log"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        if !log.trim().is_empty() {
            self.console.write(&format!(
                "\nThe end of ~/simplewm.log:\n\n{}\n",
                tail_lines(&log, LOG_TAIL)
            ));
        }
        self.console.write(&format!(
            "Here's a shell as {}. Run simplewm to see what it says, or exit the shell to \
             have simplewm-init start it again (its output then goes to ~/simplewm.log).\n\n",
            self.user.name
        ));
        match spawn_shell(&self.user, &self.runtime) {
            Ok(pid) => self.shell = Some(pid),
            Err(e) => {
                self.console.say(&format!(
                    "can't start a shell ({e}); trying simplewm again in {} s",
                    NO_SHELL_RETRY.as_secs()
                ));
                self.start_at = Some(Instant::now() + NO_SHELL_RETRY);
            }
        }
    }

    fn shutdown(&mut self, signals: &sys::Signals, how: libc::c_int, what: &str) -> ! {
        self.console.say(what);
        self.start_at = None;
        // An interactive bash ignores SIGTERM, and the user's shell may be the one
        // running `poweroff`: end its session at once.
        if let Some(pid) = self.shell {
            sys::kill_session(pid);
        }
        sys::kill_all(libc::SIGTERM);
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            while sys::reap().is_some() {}
            if !sys::has_children() {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.console.say(&format!(
                    "still running after 5 s, killing: {}",
                    sys::other_processes().join(", ")
                ));
                sys::kill_all(libc::SIGKILL);
                std::thread::sleep(Duration::from_millis(200));
                while sys::reap().is_some() {}
                break;
            }
            sys::poll(&[signals.fd()], left.as_millis() as i32);
            while signals.next().is_some() {}
        }
        sys::sync();
        if sys::is_mount_point(home::MOUNT_POINT)
            && let Err(e) = sys::unmount(home::MOUNT_POINT)
        {
            self.console
                .say(&format!("can't unmount {}: {e}", home::MOUNT_POINT));
        }
        if let Err(e) = sys::remount_root_read_only() {
            self.console.say(&format!("can't remount / read-only: {e}"));
        }
        let e = sys::reboot(how);
        self.console.say(&format!("{what} failed: {e}"));
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
}

/// A command run as the user: their IDs and groups, their home folder, and a fresh
/// login-like set of variables. simplewm and the shell given after failures are both
/// made this way (and both go through the login shell), so running simplewm in that
/// shell is the same as simplewm-init running it. With
/// `tty`, that terminal (opened while still root) becomes its stdin, stdout, stderr and
/// controlling terminal.
fn user_command(program: &Path, user: &Account, runtime: &Path, tty: Option<&str>) -> Command {
    let mut cmd = command(program);
    cmd.env_clear()
        .env("HOME", &user.home)
        .env("USER", &user.name)
        .env("LOGNAME", &user.name)
        .env("SHELL", &user.shell)
        .env("PATH", PATH)
        .env("XDG_RUNTIME_DIR", runtime)
        .current_dir(&user.home);
    let (uid, gid, groups) = (user.uid, user.gid, user.groups.clone());
    let tty = tty.map(|t| CString::new(t).expect("no NUL in a fixed path"));
    // SAFETY: only async-signal-safe calls (setsid, open, ioctl, dup2, close, setgroups,
    // setgid, setuid).
    unsafe {
        cmd.pre_exec(move || {
            // A new session, which can take a terminal.
            libc::setsid();
            if let Some(tty) = &tty {
                let fd: RawFd = libc::open(tty.as_ptr(), libc::O_RDWR);
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                libc::ioctl(fd, libc::TIOCSCTTY as _, 0);
                for target in 0..3 {
                    libc::dup2(fd, target);
                }
                if fd > 2 {
                    libc::close(fd);
                }
            }
            if libc::setgroups(groups.len(), groups.as_ptr()) < 0
                || libc::setgid(gid) < 0
                || libc::setuid(uid) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd
}

/// simplewm as the user, its output appended to ~/simplewm.log. It goes through the
/// user's login shell (which replaces itself with simplewm), so it gets what the login
/// profile sets (LANG and so on), exactly like running `simplewm` in the shell given
/// after failures.
fn spawn_simplewm(user: &Account, runtime: &Path) -> io::Result<i32> {
    let mut cmd = user_command(&user.shell, user, runtime, None);
    cmd.args(["-l", "-c", "exec simplewm"]).stdin(Stdio::null());
    if let Some((out, err)) = log_file(&user.home.join("simplewm.log"), Some(user)) {
        cmd.stdout(out).stderr(err);
    }
    Ok(cmd.spawn()?.id() as i32)
}

/// The user's login shell on tty1. Like a login, the terminal is theirs while it runs.
fn spawn_shell(user: &Account, runtime: &Path) -> io::Result<i32> {
    let _ = std::os::unix::fs::chown(SCREEN_TTY, Some(user.uid), None);
    let mut cmd = user_command(&user.shell, user, runtime, Some(SCREEN_TTY));
    cmd.arg("-l").env("TERM", "linux");
    Ok(cmd.spawn()?.id() as i32)
}

/// The last `n` lines of `text`, each ending in a newline.
fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| format!("{l}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::tail_lines;

    #[test]
    fn tails_lines() {
        assert_eq!(tail_lines("a\nb\nc\n", 2), "b\nc\n");
        assert_eq!(tail_lines("a\nb", 5), "a\nb\n");
        assert_eq!(tail_lines("", 3), "");
    }
}
