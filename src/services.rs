//! The user services simplewm runs itself, in place of `systemd --user`: the D-Bus
//! session bus (`dbus-daemon`), PipeWire, its PulseAudio server (`pipewire-pulse`) and
//! WirePlumber. No UI here; the Sound and D-Bus applets show and control them.
//!
//! This is deliberately small: a fixed list of programs simplewm always needs, started in
//! order on a TTY, stopped when simplewm quits, with their output kept as a log. Nothing
//! restarts them when they exit, and there are no unit files.
//!
//! If a service is already running (started by systemd, or a host desktop when nested),
//! simplewm leaves it alone and shows who runs it: whoever answers on the service's socket
//! (the kernel says who's listening), or for WirePlumber, which has no socket, a process
//! of that name owned by us.
//!
//! Each started program gets three threads: one reads its output (stdout and stderr share
//! one pipe), one waits for it to exit, and one waits for its socket to answer. They report
//! back through `Notify`, which `State` wires to a calloop channel. Each start gets a new
//! `run` number, so events from a previous run of the same service are told apart.
//!
//! The waiter first waits for the exit without reaping (`WNOWAIT`), clears the shared PID,
//! and only then reaps. `stop` signals only while holding that PID's lock, so it can never
//! signal a recycled PID.

use std::{
    collections::VecDeque,
    io::{BufRead, BufReader},
    os::unix::{
        fs::MetadataExt,
        net::UnixStream,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Lines of output kept per service.
const LOG_LINES: usize = 2000;
/// How long a stopped service gets to exit before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// How long to wait for a new service's socket to answer.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// WirePlumber has no socket; it has usually set up the sound cards by then.
const WIREPLUMBER_SETTLE: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    Bus,
    PipeWire,
    Pulse,
    WirePlumber,
}

impl Service {
    /// Start order.
    pub const ALL: [Service; 4] = [
        Service::Bus,
        Service::PipeWire,
        Service::Pulse,
        Service::WirePlumber,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Service::Bus => "D-Bus",
            Service::PipeWire => "PipeWire",
            Service::Pulse => "PipeWire Pulse",
            Service::WirePlumber => "WirePlumber",
        }
    }

    pub fn program(self) -> &'static str {
        match self {
            Service::Bus => "dbus-daemon",
            Service::PipeWire => "pipewire",
            Service::Pulse => "pipewire-pulse",
            Service::WirePlumber => "wireplumber",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// What has to be up before simplewm starts this one at login.
    fn after(self) -> Option<Service> {
        match self {
            Service::Bus => None,
            Service::PipeWire => Some(Service::Bus),
            Service::Pulse | Service::WirePlumber => Some(Service::PipeWire),
        }
    }

    /// The socket it serves clients on, if any.
    fn socket(self, runtime: &Path) -> Option<PathBuf> {
        match self {
            Service::Bus => bus_address().as_deref().and_then(bus_socket),
            Service::PipeWire => Some(runtime.join("pipewire-0")),
            Service::Pulse => Some(runtime.join("pulse/native")),
            Service::WirePlumber => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Stopped,
    /// Started, its socket not answering yet.
    Starting,
    Running,
    /// Asked to stop, not exited yet.
    Stopping,
    /// Exited on its own, with how.
    Exited(String),
    /// Couldn't be started, with why.
    Failed(String),
    /// Run by another program (described), so simplewm leaves it alone.
    Elsewhere(String),
}

/// What a service's threads report.
pub struct ServiceEvent {
    pub service: Service,
    run: u64,
    pub kind: EventKind,
}

pub enum EventKind {
    /// A line of the program's output, or a note from simplewm about it.
    Line(String),
    /// Its socket answers (or it never did and we stopped waiting).
    Ready,
    /// It exited, described ("exited with status 1").
    Exited(String),
}

/// Where the threads send their events (to the compositor's event loop).
pub type Notify = Arc<dyn Fn(ServiceEvent) + Send + Sync>;

struct Proc {
    status: Status,
    /// While it runs. Locked around signalling and reaping (see the module docs).
    pid: Arc<Mutex<Option<u32>>>,
    /// Number of the current (or last) start.
    run: u64,
    log: VecDeque<String>,
}

impl Default for Proc {
    fn default() -> Self {
        Proc {
            status: Status::Stopped,
            pid: Arc::new(Mutex::new(None)),
            run: 0,
            log: VecDeque::new(),
        }
    }
}

pub struct Services {
    procs: [Proc; 4],
    notify: Notify,
    /// For the bus's environment, so services it starts can open windows.
    wayland_display: String,
    /// Services still to start at login, each once what it needs is up.
    pending: Vec<Service>,
}

impl Services {
    pub fn new(notify: Notify, wayland_display: String) -> Self {
        Services {
            procs: Default::default(),
            notify,
            wayland_display,
            pending: Vec::new(),
        }
    }

    pub fn status(&self, s: Service) -> &Status {
        &self.procs[s.index()].status
    }

    pub fn pid(&self, s: Service) -> Option<u32> {
        *self.procs[s.index()].pid.lock().ok()?
    }

    /// The service's output and simplewm's notes about it, oldest first.
    pub fn log(&self, s: Service) -> &VecDeque<String> {
        &self.procs[s.index()].log
    }

    /// One line for the applets: "Running (PID 123)".
    pub fn describe(&self, s: Service) -> String {
        match self.status(s) {
            Status::Stopped => "Stopped".into(),
            Status::Starting => "Starting…".into(),
            Status::Running => match self.pid(s) {
                Some(pid) => format!("Running (PID {pid})"),
                None => "Running".into(),
            },
            Status::Stopping => "Stopping…".into(),
            Status::Exited(how) => format!("Stopped: {how}"),
            Status::Failed(why) => format!("Couldn't start: {why}"),
            Status::Elsewhere(who) => format!("Run by {who}"),
        }
    }

    /// Whether the applets offer Start (true) or Stop (false), or neither (None).
    pub fn can_start(&self, s: Service) -> Option<bool> {
        match self.status(s) {
            Status::Stopped | Status::Exited(_) | Status::Failed(_) => Some(true),
            Status::Starting | Status::Running => Some(false),
            Status::Stopping | Status::Elsewhere(_) => None,
        }
    }

    /// Start everything that isn't already running, in order (on a TTY, at startup).
    pub fn autostart(&mut self) {
        self.pending = Service::ALL.to_vec();
        self.advance();
    }

    /// Start the pending services whose dependency has settled (up, or given up on).
    fn advance(&mut self) {
        while let Some(i) = self.next_ready() {
            let s = self.pending.remove(i);
            self.start(s);
        }
    }

    /// The first pending service whose dependency has settled. A dependency that is
    /// still pending itself hasn't: it hasn't even been tried yet.
    fn next_ready(&self) -> Option<usize> {
        self.pending.iter().position(|s| {
            s.after().is_none_or(|dep| {
                !self.pending.contains(&dep) && !matches!(self.status(dep), Status::Starting)
            })
        })
    }

    /// Re-check who runs the services simplewm isn't running (when an applet opens).
    pub fn refresh(&mut self) {
        let Some(runtime) = runtime_dir() else { return };
        for s in Service::ALL {
            let p = &mut self.procs[s.index()];
            match (&p.status, other_instance(s, &runtime)) {
                (Status::Starting | Status::Running | Status::Stopping, _) => {}
                (_, Some(who)) => p.status = Status::Elsewhere(who),
                (Status::Elsewhere(_), None) => p.status = Status::Stopped,
                _ => {}
            }
        }
    }

    pub fn start(&mut self, s: Service) {
        if matches!(
            self.status(s),
            Status::Starting | Status::Running | Status::Stopping
        ) {
            return;
        }
        let Some(runtime) = runtime_dir() else {
            self.procs[s.index()].status = Status::Failed("XDG_RUNTIME_DIR is not set".into());
            return;
        };
        if let Some(who) = other_instance(s, &runtime) {
            tracing::info!(service = s.name(), who, "service already running elsewhere");
            self.procs[s.index()].status = Status::Elsewhere(who);
            return;
        }
        match self.spawn(s, &runtime) {
            Ok(()) => {}
            Err(why) => {
                tracing::warn!(service = s.name(), why, "cannot start service");
                self.note(
                    s,
                    format!("simplewm: couldn't start {}: {why}", s.program()),
                );
                self.procs[s.index()].status = Status::Failed(why);
            }
        }
    }

    fn spawn(&mut self, s: Service, runtime: &Path) -> Result<(), String> {
        let mut cmd = Command::new(s.program());
        let bus = bus_address();
        if let Some(bus) = &bus {
            cmd.env("DBUS_SESSION_BUS_ADDRESS", bus);
        }
        if s == Service::Bus {
            let bus = bus.ok_or("no bus address (XDG_RUNTIME_DIR is not set)")?;
            // Nothing answered on the socket (see other_instance), so a file left there
            // is stale, and dbus-daemon won't listen over it.
            if let Some(path) = bus_socket(&bus) {
                let _ = std::fs::remove_file(path);
            }
            cmd.args(["--session", "--nofork", "--nopidfile", "--nosyslog"])
                .arg(format!("--address={bus}"))
                // Programs the bus starts on demand get these.
                .env("WAYLAND_DISPLAY", &self.wayland_display)
                .env("XDG_CURRENT_DESKTOP", "simplewm")
                .env_remove("DISPLAY");
        }

        let (reader, writer) = std::io::pipe().map_err(|e| format!("cannot make a pipe: {e}"))?;
        let writer2 = writer
            .try_clone()
            .map_err(|e| format!("cannot make a pipe: {e}"))?;
        cmd.stdin(Stdio::null())
            .stdout(writer)
            .stderr(writer2)
            // Its own process group, so signals meant for simplewm's terminal skip it.
            .process_group(0);
        // SAFETY: prctl is async-signal-safe, and nothing else runs in the child before
        // exec. The main thread spawns, and it lives as long as simplewm, so the services
        // get SIGTERM if simplewm dies without stopping them.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| e.to_string())?;
        // Drop our copies of the pipe's write end, or the reader never sees the end.
        drop(cmd);

        let pid = child.id();
        let p = &mut self.procs[s.index()];
        p.run += 1;
        p.status = Status::Starting;
        p.pid = Arc::new(Mutex::new(Some(pid)));
        let run = p.run;
        let slot = p.pid.clone();
        tracing::info!(service = s.name(), pid, "started service");
        self.note(s, format!("simplewm: started {} (PID {pid})", s.program()));

        let send = {
            let notify = self.notify.clone();
            move |kind| {
                notify(ServiceEvent {
                    service: s,
                    run,
                    kind,
                })
            }
        };
        let name = |what: &str| format!("{}-{what}", s.program());

        let out = send.clone();
        spawn_thread(name("log"), move || {
            let mut reader = BufReader::new(reader);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => out(EventKind::Line(clean_line(&buf))),
                }
            }
        });

        let done = send.clone();
        let waited = slot.clone();
        spawn_thread(name("wait"), move || {
            let how = wait_for(child, &waited);
            done(EventKind::Exited(how));
        });

        let socket = s.socket(runtime);
        spawn_thread(name("ready"), move || {
            let still_running = || slot.lock().map(|p| p.is_some()).unwrap_or(false);
            let Some(socket) = socket else {
                std::thread::sleep(WIREPLUMBER_SETTLE);
                if still_running() {
                    send(EventKind::Ready);
                }
                return;
            };
            let start = Instant::now();
            while still_running() {
                if UnixStream::connect(&socket).is_ok() {
                    send(EventKind::Ready);
                    return;
                }
                if start.elapsed() > READY_TIMEOUT {
                    send(EventKind::Line(format!(
                        "simplewm: nothing answers on {} yet; carrying on anyway",
                        socket.display()
                    )));
                    send(EventKind::Ready);
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        Ok(())
    }

    /// Ask a service simplewm started to stop: SIGTERM, then SIGKILL after `STOP_GRACE`.
    pub fn stop(&mut self, s: Service) {
        if !matches!(self.status(s), Status::Starting | Status::Running) {
            return;
        }
        let p = &mut self.procs[s.index()];
        let Some(pid) = signal(&p.pid, libc::SIGTERM) else {
            return;
        };
        p.status = Status::Stopping;
        let slot = p.pid.clone();
        let run = p.run;
        tracing::info!(service = s.name(), pid, "stopping service");
        self.note(s, format!("simplewm: stopping {} (PID {pid})", s.program()));
        let notify = self.notify.clone();
        spawn_thread(format!("{}-stop", s.program()), move || {
            std::thread::sleep(STOP_GRACE);
            if signal(&slot, libc::SIGKILL) == Some(pid) {
                notify(ServiceEvent {
                    service: s,
                    run,
                    kind: EventKind::Line(format!(
                        "simplewm: still running after {} s; killed it",
                        STOP_GRACE.as_secs()
                    )),
                });
            }
        });
    }

    /// Stop everything simplewm started and wait (briefly) for it to go, as simplewm quits.
    pub fn shutdown(&mut self) {
        let slots: Vec<_> = self.procs.iter().map(|p| p.pid.clone()).collect();
        for slot in slots.iter().rev() {
            signal(slot, libc::SIGTERM);
        }
        let running = || {
            slots
                .iter()
                .any(|s| s.lock().map(|p| p.is_some()).unwrap_or(false))
        };
        let start = Instant::now();
        while running() && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(20));
        }
        for slot in &slots {
            signal(slot, libc::SIGKILL);
        }
    }

    /// Take in an event from a service's threads. Returns whether its status changed.
    pub fn handle(&mut self, ev: ServiceEvent) -> bool {
        let s = ev.service;
        let current = ev.run == self.procs[s.index()].run;
        let changed = match ev.kind {
            EventKind::Line(line) => {
                self.note(s, line);
                false
            }
            EventKind::Ready if current => {
                let p = &mut self.procs[s.index()];
                let starting = p.status == Status::Starting;
                if starting {
                    p.status = Status::Running;
                }
                starting
            }
            EventKind::Exited(how) if current => {
                let p = &mut self.procs[s.index()];
                let asked = p.status == Status::Stopping;
                p.status = if asked {
                    Status::Stopped
                } else {
                    Status::Exited(how.clone())
                };
                tracing::info!(service = s.name(), how, "service exited");
                self.note(s, format!("simplewm: {} {how}", s.program()));
                true
            }
            EventKind::Ready | EventKind::Exited(_) => false,
        };
        if changed {
            self.advance();
        }
        changed
    }

    fn note(&mut self, s: Service, line: String) {
        let log = &mut self.procs[s.index()].log;
        if log.len() == LOG_LINES {
            log.pop_front();
        }
        log.push_back(format!("{}  {line}", crate::clock::now("%H:%M:%S")));
    }
}

/// Send `sig` to the process in `slot` if it's still running; returns its PID.
fn signal(slot: &Mutex<Option<u32>>, sig: i32) -> Option<u32> {
    let guard = slot.lock().ok()?;
    let pid = (*guard)?;
    // SAFETY: plain syscall. The lock keeps the waiter from reaping meanwhile, so the PID
    // is still this child's (see the module docs).
    unsafe { libc::kill(pid as i32, sig) };
    Some(pid)
}

/// Wait for the child to exit, clear its PID, then reap it. Describes how it ended.
fn wait_for(mut child: std::process::Child, slot: &Mutex<Option<u32>>) -> String {
    let pid = child.id();
    loop {
        // SAFETY: waitid only writes into `info`. WNOWAIT leaves the child unreaped.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let r = unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if r == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            break;
        }
    }
    if let Ok(mut p) = slot.lock() {
        *p = None;
    }
    match child.wait() {
        Ok(status) => describe_exit(status),
        Err(err) => format!("exited (couldn't tell how: {err})"),
    }
}

fn describe_exit(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(0), _) => "exited".into(),
        (Some(code), _) => format!("exited with status {code}"),
        (None, Some(libc::SIGTERM)) => "was stopped (SIGTERM)".into(),
        (None, Some(libc::SIGKILL)) => "was killed (SIGKILL)".into(),
        (None, Some(sig)) => format!("crashed (signal {sig})"),
        (None, None) => "exited".into(),
    }
}

/// A line of output as text: invalid UTF-8 replaced, tabs widened, other control
/// characters (colors, the newline) dropped.
fn clean_line(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => out.push_str("    "),
            // ESC [ … letter: a terminal color code.
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn spawn_thread(name: String, f: impl FnOnce() + Send + 'static) {
    if let Err(err) = std::thread::Builder::new().name(name).spawn(f) {
        tracing::warn!(?err, "services: cannot start thread");
    }
}

fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

/// The session bus address: what the session was given, else the usual socket in
/// XDG_RUNTIME_DIR (what zbus and libdbus fall back to as well).
pub fn bus_address() -> Option<String> {
    std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .ok()
        .filter(|a| !a.is_empty())
        .or_else(|| runtime_dir().map(|d| format!("unix:path={}/bus", d.display())))
}

/// The socket file of a `unix:path=…` bus address (the first one, if several).
fn bus_socket(address: &str) -> Option<PathBuf> {
    let first = address.split(';').next()?;
    let params = first.strip_prefix("unix:")?;
    params
        .split(',')
        .find_map(|p| p.strip_prefix("path="))
        .map(PathBuf::from)
}

/// Who already runs service `s`, if anyone: whoever listens on its socket, or for a
/// service with no socket, a process of that name owned by us.
fn other_instance(s: Service, runtime: &Path) -> Option<String> {
    let Some(socket) = s.socket(runtime) else {
        return find_process(s.program()).map(describe_pid);
    };
    // A service with a socket is running if and only if something answers there: a
    // dbus-daemon serving some other bus (an accessibility bus, say) isn't our bus.
    let stream = UnixStream::connect(&socket).ok()?;
    Some(match peer_pid(&stream) {
        Some(pid) => describe_pid(pid),
        None => "another program".into(),
    })
}

/// PID of the process on the other end of a Unix socket.
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes at most `len` bytes into `cred`.
    let r = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    (r == 0 && cred.pid > 0).then_some(cred.pid as u32)
}

/// A process of ours (other than simplewm) whose name (`comm`) is `name`.
pub fn find_process(name: &str) -> Option<u32> {
    // SAFETY: getuid can't fail.
    let uid = unsafe { libc::getuid() };
    let me = std::process::id();
    std::fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
        let pid: u32 = e.file_name().to_str()?.parse().ok()?;
        if pid == me || e.metadata().ok()?.uid() != uid {
            return None;
        }
        let comm = std::fs::read_to_string(e.path().join("comm")).ok()?;
        (comm.trim_end() == name).then_some(pid)
    })
}

/// The program running as `pid`: its command line's first word, else its `comm`
/// (empty if unknown).
pub fn process_name(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .ok()
        .and_then(|c| c.split('\0').next().map(str::to_string))
        .filter(|c| !c.is_empty())
        .or_else(|| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok())
        .map(|c| c.trim().to_string())
        .unwrap_or_default()
}

/// `PID 123 (/usr/lib/xdg-desktop-portal)`.
pub fn describe_pid(pid: u32) -> String {
    match process_name(pid) {
        name if name.is_empty() => format!("PID {pid}"),
        name => format!("PID {pid} ({name})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn starts_in_dependency_order() {
        let mut services = Services::new(Arc::new(|_| {}), String::new());
        services.pending = Service::ALL.to_vec();
        let next = |services: &Services| services.next_ready().map(|i| services.pending[i]);

        // Only the bus can go first; PipeWire's users wait while PipeWire is pending.
        assert_eq!(next(&services), Some(Service::Bus));
        services.pending.retain(|s| *s != Service::Bus);
        services.procs[Service::Bus.index()].status = Status::Starting;
        assert_eq!(next(&services), None);

        services.procs[Service::Bus.index()].status = Status::Running;
        assert_eq!(next(&services), Some(Service::PipeWire));
        services.pending.retain(|s| *s != Service::PipeWire);
        services.procs[Service::PipeWire.index()].status = Status::Starting;
        assert_eq!(next(&services), None);

        // A dependency that failed has settled too: the rest still get their try.
        services.procs[Service::PipeWire.index()].status = Status::Failed("x".into());
        assert!(next(&services).is_some());
    }

    #[test]
    fn reads_bus_socket() {
        assert_eq!(
            bus_socket("unix:path=/run/user/1000/bus"),
            Some(PathBuf::from("/run/user/1000/bus"))
        );
        assert_eq!(
            bus_socket("unix:guid=abc,path=/tmp/b;tcp:host=x"),
            Some(PathBuf::from("/tmp/b"))
        );
        assert_eq!(bus_socket("unix:abstract=/tmp/dbus-x"), None);
        assert_eq!(bus_socket("tcp:host=localhost,port=1"), None);
    }

    #[test]
    fn cleans_lines() {
        assert_eq!(clean_line(b"plain\n"), "plain");
        assert_eq!(clean_line(b"a\tb"), "a    b");
        assert_eq!(clean_line(b"\x1b[1;31mE\x1b[0m error\r\n"), "E error");
        assert_eq!(clean_line(b"bad \xff byte"), "bad \u{fffd} byte");
    }

    #[test]
    fn finds_processes() {
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        // The child is only named sleep once it has exec'd.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while find_process("sleep").is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(find_process("sleep").is_some());
        assert!(describe_pid(child.id()).contains("sleep"));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(find_process("no-such-program-here"), None);
    }

    #[test]
    fn describes_exits() {
        let run =
            |script: &str| describe_exit(Command::new("sh").args(["-c", script]).status().unwrap());
        assert_eq!(run("exit 0"), "exited");
        assert_eq!(run("exit 3"), "exited with status 3");
        assert_eq!(run("kill -TERM $$"), "was stopped (SIGTERM)");
        assert_eq!(run("kill -SEGV $$"), "crashed (signal 11)");
    }

    #[test]
    fn waits_without_racing_the_pid() {
        let child = Command::new("sh").args(["-c", "exit 2"]).spawn().unwrap();
        let slot = Mutex::new(Some(child.id()));
        assert_eq!(wait_for(child, &slot), "exited with status 2");
        assert_eq!(*slot.lock().unwrap(), None);
        assert_eq!(signal(&slot, libc::SIGTERM), None);
    }

    /// Starts a real dbus-daemon on a scratch socket and stops it again.
    /// Needs dbus-daemon: `cargo test runs_the_session_daemon -- --ignored`. Not named
    /// "bus": `cargo test bus` runs the portal tests, and this one changes the
    /// environment they read.
    #[test]
    #[ignore]
    fn runs_the_session_daemon() {
        let dir = std::env::temp_dir().join(format!("simplewm-services-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: this test runs alone (it's ignored by default); nothing reads the
        // environment concurrently.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
            std::env::set_var(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/bus", dir.display()),
            );
        }
        let (tx, rx) = mpsc::channel::<ServiceEvent>();
        let tx = Mutex::new(tx);
        let notify: Notify = Arc::new(move |ev| {
            let _ = tx.lock().unwrap().send(ev);
        });
        let mut services = Services::new(notify, "wayland-test".into());
        services.start(Service::Bus);
        assert_eq!(services.status(Service::Bus), &Status::Starting);
        let until = |services: &mut Services, want: Status| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while services.status(Service::Bus) != &want {
                let left = deadline.saturating_duration_since(Instant::now());
                let ev = rx.recv_timeout(left).expect("no event in time");
                services.handle(ev);
            }
        };
        until(&mut services, Status::Running);
        assert!(dir.join("bus").exists());
        // A second start sees it running.
        let elsewhere = other_instance(Service::Bus, &dir);
        assert!(elsewhere.unwrap().contains("dbus-daemon"));
        services.stop(Service::Bus);
        until(&mut services, Status::Stopped);
        let log: Vec<_> = services.log(Service::Bus).iter().cloned().collect();
        assert!(
            log.iter().any(|l| l.contains("started dbus-daemon")),
            "{log:?}"
        );
        assert!(log.iter().any(|l| l.contains("stopping")), "{log:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
