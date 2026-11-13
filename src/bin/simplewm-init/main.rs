//! simplewm-init: PID 1 for the simplewm distro (see `distro/DISTRO.md`).
//!
//! It mounts the basic filesystems, starts udevd (devices) and seatd (screen and input
//! access) as root, then runs simplewm as the user (the account with UID 1000) and keeps
//! it running. There is nothing to configure.
//!
//! The screen has one VT, tty1, and only simplewm runs there. When simplewm exits, the
//! kernel's text console comes back on tty1 and simplewm-init writes why there. After
//! three exits within a minute it stops restarting simplewm, shows the end of its log and
//! waits for Enter on tty1 to try again, as often as needed.
//!
//! For development, a root shell with no password runs on the serial port (ttyS0): in
//! QEMU that's the terminal running it. On a PC with nothing plugged into the serial port
//! it does nothing.
//!
//! Shutting down: busybox's `poweroff`, `reboot` and `halt` signal PID 1 with SIGUSR2,
//! SIGTERM and SIGUSR1. Everything gets SIGTERM, then SIGKILL after 5 s, the root
//! filesystem is remounted read-only, and the machine powers off, reboots or halts.
//! Ctrl+Alt+Del does nothing (no keyboard shortcuts).

mod account;
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
const SERIAL_TTY: &str = "/dev/ttyS0";
/// The screen's one VT.
const SCREEN_TTY: &str = "/dev/tty1";
/// Exits within `FAILURE_WINDOW` that make simplewm-init stop and wait for Enter.
const FAILURES: usize = 3;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const SIMPLEWM_RESTART: Duration = Duration::from_secs(2);
const SHELL_RESTART: Duration = Duration::from_secs(1);
const STOP_GRACE: Duration = Duration::from_secs(5);
/// The screen's console font (Terminus 16x32, from font-terminus), loaded with setfont.
const CONSOLE_FONT: &str = "/usr/share/consolefonts/ter-132n.psf.gz";
/// Lines of simplewm's log shown when it keeps failing.
const LOG_TAIL: usize = 20;

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
    let root_shell = Account::load(0)
        .map(|a| a.shell)
        .unwrap_or_else(|_| "/bin/sh".into());
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
        root_shell,
        simplewm: Job::due_now(),
        serial: if Path::new(SERIAL_TTY).exists() {
            Job::due_now()
        } else {
            Job::default()
        },
        exits: Vec::new(),
        waiting: None,
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

/// A program simplewm-init keeps running.
#[derive(Default)]
struct Job {
    pid: Option<i32>,
    /// When to start it (again); `None` while it runs, or while waiting for Enter.
    start_at: Option<Instant>,
}

impl Job {
    fn due_now() -> Job {
        Job {
            pid: None,
            start_at: Some(Instant::now()),
        }
    }
}

struct Supervisor {
    console: Console,
    user: Account,
    runtime: PathBuf,
    root_shell: PathBuf,
    simplewm: Job,
    serial: Job,
    /// When simplewm last exited, within `FAILURE_WINDOW`.
    exits: Vec<Instant>,
    /// tty1, read for Enter after simplewm kept failing.
    waiting: Option<File>,
}

impl Supervisor {
    fn run(&mut self, signals: &sys::Signals) {
        loop {
            self.start_due();
            let timeout = [self.simplewm.start_at, self.serial.start_at]
                .into_iter()
                .flatten()
                .min()
                .map(|t| t.saturating_duration_since(Instant::now()).as_millis() as i32)
                .unwrap_or(-1);
            let mut fds = vec![signals.fd()];
            fds.extend(self.waiting.as_ref().map(|f| f.as_raw_fd()));
            let ready = sys::poll(&fds, timeout);

            while let Some(signal) = signals.next() {
                match signal {
                    libc::SIGCHLD => self.reap(),
                    libc::SIGUSR2 => self.shutdown(signals, libc::RB_POWER_OFF, "powering off"),
                    libc::SIGTERM => self.shutdown(signals, libc::RB_AUTOBOOT, "rebooting"),
                    libc::SIGUSR1 => self.shutdown(signals, libc::RB_HALT_SYSTEM, "halting"),
                    _ => {} // SIGINT: Ctrl+Alt+Del, which does nothing.
                }
            }
            if ready.get(1) == Some(&true) {
                self.read_enter();
            }
        }
    }

    fn start_due(&mut self) {
        let now = Instant::now();
        if self.serial.start_at.is_some_and(|t| t <= now) {
            self.serial.start_at = None;
            match spawn_shell(&self.root_shell, SERIAL_TTY) {
                Ok(pid) => self.serial.pid = Some(pid),
                Err(e) => {
                    self.console
                        .say(&format!("can't start the serial shell: {e}"));
                    self.serial.start_at = Some(now + SHELL_RESTART * 10);
                }
            }
        }
        if self.simplewm.start_at.is_some_and(|t| t <= now) {
            self.simplewm.start_at = None;
            self.console.say(&format!(
                "starting simplewm as {} (log: ~/simplewm.log)",
                self.user.name
            ));
            match spawn_simplewm(&self.user, &self.runtime) {
                Ok(pid) => self.simplewm.pid = Some(pid),
                Err(e) => self.simplewm_ended(&format!("couldn't be started ({e})")),
            }
        }
    }

    fn reap(&mut self) {
        while let Some((pid, status)) = sys::reap() {
            if self.serial.pid == Some(pid) {
                self.serial.pid = None;
                self.serial.start_at = Some(Instant::now() + SHELL_RESTART);
            } else if self.simplewm.pid == Some(pid) {
                self.simplewm.pid = None;
                self.simplewm_ended(&sys::describe_status(status));
            }
            // Anything else is an orphan that ended up with us; reaping it is enough.
        }
    }

    /// simplewm is gone (`how` follows "simplewm": "exited with status 1"): start it
    /// again shortly, or after too many exits, explain on tty1 and wait for Enter.
    fn simplewm_ended(&mut self, how: &str) {
        let now = Instant::now();
        self.exits
            .retain(|t| now.duration_since(*t) < FAILURE_WINDOW);
        self.exits.push(now);
        if self.exits.len() < FAILURES {
            self.console.say(&format!(
                "simplewm {how}; starting it again in {} s",
                SIMPLEWM_RESTART.as_secs()
            ));
            self.simplewm.start_at = Some(now + SIMPLEWM_RESTART);
            return;
        }
        let log = self.user.home.join("simplewm.log");
        let tail = log_tail(&log, LOG_TAIL);
        // Clear the screen so the report starts at the top.
        self.console.write("\x1b[H\x1b[2J");
        self.console.say(&format!(
            "simplewm {how}. It stopped {FAILURES} times within a minute, so it won't be \
             started again until you press Enter."
        ));
        self.console
            .write(&format!("\nThe end of {}:\n\n{tail}\n", log.display()));
        self.console.write("Press Enter to start simplewm again.\n");
        self.waiting = open_for_enter();
        if self.waiting.is_none() {
            self.console
                .say("can't read tty1; trying again in a minute");
            self.simplewm.start_at = Some(now + FAILURE_WINDOW);
        }
    }

    fn read_enter(&mut self) {
        let Some(tty) = self.waiting.as_mut() else {
            return;
        };
        let mut buf = [0u8; 256];
        match tty.read(&mut buf) {
            Ok(n) if buf[..n].contains(&b'\n') => {
                self.waiting = None;
                self.exits.clear();
                self.simplewm.start_at = Some(Instant::now());
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(_) => self.waiting = open_for_enter(),
        }
    }

    fn shutdown(&mut self, signals: &sys::Signals, how: libc::c_int, what: &str) -> ! {
        self.console.say(what);
        self.waiting = None;
        sys::kill_all(libc::SIGTERM);
        // The serial shell has nothing to save, and an interactive bash ignores SIGTERM
        // (and may be busy running `poweroff` itself): end its session at once.
        if let Some(pid) = self.serial.pid {
            sys::kill_session(pid);
        }
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

/// tty1 for reading Enter, with anything typed earlier thrown away.
fn open_for_enter() -> Option<File> {
    let tty = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(SCREEN_TTY)
        .ok()?;
    // SAFETY: tcflush on a tty we just opened.
    unsafe { libc::tcflush(tty.as_raw_fd(), libc::TCIFLUSH) };
    Some(tty)
}

/// The last `n` lines of `path`, or a note why there are none.
fn log_tail(path: &Path, n: usize) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(n);
            if lines.is_empty() {
                "(empty)\n".into()
            } else {
                lines[start..].join("\n") + "\n"
            }
        }
        Err(e) => format!("(can't read it: {e})\n"),
    }
}

/// A root login shell on `tty`, which becomes its controlling terminal.
fn spawn_shell(shell: &Path, tty: &str) -> io::Result<i32> {
    let tty = CString::new(tty).expect("no NUL in a fixed path");
    let mut cmd = command(shell);
    cmd.arg("-l")
        .env_clear()
        .envs([
            ("HOME", "/root"),
            ("USER", "root"),
            ("LOGNAME", "root"),
            ("PATH", PATH),
            ("TERM", "linux"),
        ])
        .env("SHELL", shell)
        .current_dir("/root");
    // SAFETY: only async-signal-safe calls (setsid, open, ioctl, dup2, close).
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
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
            Ok(())
        });
    }
    Ok(cmd.spawn()?.id() as i32)
}

/// simplewm as the user: their IDs and groups, a fresh environment like a login's, and
/// output appended to ~/simplewm.log.
fn spawn_simplewm(user: &Account, runtime: &Path) -> io::Result<i32> {
    let mut cmd = command("/usr/bin/simplewm");
    cmd.env_clear()
        .env("HOME", &user.home)
        .env("USER", &user.name)
        .env("LOGNAME", &user.name)
        .env("SHELL", &user.shell)
        .env("PATH", PATH)
        .env("XDG_RUNTIME_DIR", runtime)
        .current_dir(&user.home)
        .stdin(Stdio::null());
    if let Some((out, err)) = log_file(&user.home.join("simplewm.log"), Some(user)) {
        cmd.stdout(out).stderr(err);
    }
    let (uid, gid, groups) = (user.uid, user.gid, user.groups.clone());
    // SAFETY: only async-signal-safe calls (setsid, setgroups, setgid, setuid).
    unsafe {
        cmd.pre_exec(move || {
            libc::setsid();
            if libc::setgroups(groups.len(), groups.as_ptr()) < 0
                || libc::setgid(gid) < 0
                || libc::setuid(uid) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(cmd.spawn()?.id() as i32)
}
