//! simplewm as the D-Bus session bus, in place of `dbus-daemon` (see `services`, which
//! starts and stops it like the other services).
//!
//! zbus does each connection's handshake and message framing (its server side, with the
//! `bus-impl` feature). This module is the bus on top: unique names, well-known names with
//! their queues, match rules (`rule`), routing, and the bus's own methods and signals
//! (`org.freedesktop.DBus`). There is no activation, no security policy beyond "only the
//! user's own programs connect", no eavesdropping and no monitors.
//!
//! Threads: "bus-accept" accepts on the socket and checks the peer's uid; "bus" sits in
//! `zbus::block_on` for good, which is what drives every connection. The "bus" thread runs
//! one future (`Router::poll`) that polls everything itself: new sockets, handshakes, each
//! connection's incoming messages, and one send at a time per connection from that
//! connection's own queue, so a client that stops reading only holds up itself (and is
//! dropped once `MAX_QUEUED` bytes wait for it). A panic on the bus thread ends the bus,
//! not simplewm, and is reported as the service crashing.

mod rule;

use std::{
    cell::OnceCell,
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use futures_channel::mpsc;
use rule::Rule;
use zbus::{
    Connection, Message, MessageStream, Variant as Value,
    connection::{AuthMechanism, Builder},
    export::futures_core::Stream,
    message::{Flags, Header, Type},
    names::WellKnownName,
    zvariant::DynamicType,
};

/// The bus's own name, and where its methods live.
pub const NAME: &str = "org.freedesktop.DBus";
const PATH: &str = "/org/freedesktop/DBus";
const ERR: &str = "org.freedesktop.DBus.Error.";

/// Bytes waiting for one client before it's dropped for not reading them.
const MAX_QUEUED: usize = 64 << 20;
/// Match rules one client may have.
const MAX_RULES: usize = 10_000;
/// Messages taken from one client before the others get a turn.
const BATCH: usize = 32;

// RequestName flags and replies, ReleaseName replies, StartServiceByName replies.
const ALLOW_REPLACEMENT: u32 = 1;
const REPLACE_EXISTING: u32 = 2;
const DO_NOT_QUEUE: u32 = 4;
const PRIMARY_OWNER: u32 = 1;
const IN_QUEUE: u32 = 2;
const EXISTS: u32 = 3;
const ALREADY_OWNER: u32 = 4;
const RELEASED: u32 = 1;
const NON_EXISTENT: u32 = 2;
const NOT_OWNER: u32 = 3;
const ALREADY_RUNNING: u32 = 2;

/// What the bus tells `services`.
pub enum Event {
    /// A line for the D-Bus log.
    Note(String),
    /// The bus thread ended without being asked to (described).
    Stopped(String),
}

pub type Notify = Arc<dyn Fn(Event) + Send + Sync>;

/// A connected program, for the D-Bus applet.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerInfo {
    /// Its unique name (`:1.4`).
    pub name: String,
    pub pid: u32,
    /// The program (`/proc/<pid>/cmdline`'s first word), or empty.
    pub program: String,
    /// The well-known names it owns (not the ones it's queued for).
    pub names: Vec<String>,
}

/// The running bus. Dropping it stops it.
pub struct Bus {
    path: PathBuf,
    stopping: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<Incoming>,
    peers: Arc<Mutex<Vec<PeerInfo>>>,
}

enum Incoming {
    Client(UnixStream, Cred),
    Stop,
}

#[derive(Debug, Clone, Copy)]
struct Cred {
    pid: u32,
    uid: u32,
    gid: u32,
}

impl Bus {
    /// Listen on `path` (replacing a socket file left there) and serve.
    pub fn start(path: &Path, notify: Notify) -> Result<Bus, String> {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)
            .map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
        // XDG_RUNTIME_DIR is private already; this says so on the socket too.
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        let (tx, rx) = mpsc::unbounded();
        let stopping = Arc::new(AtomicBool::new(false));
        let peers = Arc::new(Mutex::new(Vec::new()));

        let bus_notify = notify.clone();
        let bus_peers = peers.clone();
        let stop_flag = stopping.clone();
        std::thread::Builder::new()
            .name("bus".into())
            .spawn(move || {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut router = Router::new(rx, bus_notify.clone(), bus_peers);
                    zbus::block_on(std::future::poll_fn(move |cx| router.poll(cx)))
                }));
                if !stop_flag.load(Ordering::SeqCst) {
                    let how = match run {
                        Err(panic) => format!("crashed ({})", panic_text(&panic)),
                        Ok(()) => "stopped".into(),
                    };
                    tracing::error!(how, "bus: thread ended");
                    bus_notify(Event::Stopped(how));
                }
            })
            .map_err(|e| format!("cannot start the bus thread: {e}"))?;

        let accept_tx = tx.clone();
        let accept_stop = stopping.clone();
        std::thread::Builder::new()
            .name("bus-accept".into())
            .spawn(move || accept(listener, accept_tx, accept_stop, notify))
            .map_err(|e| format!("cannot start the bus thread: {e}"))?;

        Ok(Bus {
            path: path.to_path_buf(),
            stopping,
            tx,
            peers,
        })
    }

    /// The connected programs, by unique name.
    pub fn peers(&self) -> Vec<PeerInfo> {
        self.peers.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Stop serving: every client is disconnected and the socket file removed.
    pub fn stop(&self) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.tx.unbounded_send(Incoming::Stop);
        // Wake the accept thread so it sees `stopping`.
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        self.stop();
    }
}

fn panic_text(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

/// The accept loop: hand each socket from one of our own programs to the bus thread.
fn accept(
    listener: UnixListener,
    tx: mpsc::UnboundedSender<Incoming>,
    stopping: Arc<AtomicBool>,
    notify: Notify,
) {
    // SAFETY: getuid can't fail.
    let me = unsafe { libc::getuid() };
    for stream in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        let stream = match stream {
            Ok(s) => s,
            Err(err) => {
                // Out of file descriptors, say: don't spin on it.
                tracing::warn!(?err, "bus: accept failed");
                std::thread::sleep(std::time::Duration::from_millis(100));
                continue;
            }
        };
        let Some(cred) = peer_cred(&stream) else {
            continue;
        };
        if cred.uid != me {
            notify(Event::Note(format!(
                "turned away PID {} (user {}): only your own programs may connect",
                cred.pid, cred.uid
            )));
            continue;
        }
        if tx.unbounded_send(Incoming::Client(stream, cred)).is_err() {
            return;
        }
    }
}

fn peer_cred(stream: &UnixStream) -> Option<Cred> {
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
    (r == 0).then_some(Cred {
        pid: cred.pid.max(0) as u32,
        uid: cred.uid,
        gid: cred.gid,
    })
}

// --- the router: connections and their queues --------------------------------------

type BoxFuture<T> = Pin<Box<dyn Future<Output = T>>>;

struct Handshake {
    id: u64,
    cred: Cred,
    building: BoxFuture<zbus::Result<Connection>>,
}

struct Conn {
    conn: Connection,
    incoming: MessageStream,
    queue: VecDeque<Message>,
    queued_bytes: usize,
    sending: Option<BoxFuture<zbus::Result<()>>>,
}

struct Router {
    rx: mpsc::UnboundedReceiver<Incoming>,
    guid: zbus::OwnedGuid,
    next_id: u64,
    handshakes: Vec<Handshake>,
    conns: HashMap<u64, Conn>,
    state: State,
    notify: Notify,
    peers: Arc<Mutex<Vec<PeerInfo>>>,
}

impl Router {
    fn new(
        rx: mpsc::UnboundedReceiver<Incoming>,
        notify: Notify,
        peers: Arc<Mutex<Vec<PeerInfo>>>,
    ) -> Router {
        let guid: zbus::OwnedGuid = zbus::Guid::generate().into();
        Router {
            rx,
            state: State::new(guid.to_string()),
            guid,
            next_id: 1,
            handshakes: Vec::new(),
            conns: HashMap::new(),
            notify,
            peers,
        }
    }

    /// One pass over everything. Returns Ready only when told to stop. When anything moved,
    /// it asks to be polled again (after the connections' own tasks have had a turn, since
    /// they run on this thread too).
    fn poll(&mut self, cx: &mut Context) -> Poll<()> {
        let mut busy = false;

        loop {
            match Pin::new(&mut self.rx).poll_next(cx) {
                Poll::Ready(Some(Incoming::Client(stream, cred))) => {
                    busy = true;
                    self.begin(stream, cred);
                }
                Poll::Ready(Some(Incoming::Stop) | None) => return Poll::Ready(()),
                Poll::Pending => break,
            }
        }

        let mut i = 0;
        while i < self.handshakes.len() {
            match self.handshakes[i].building.as_mut().poll(cx) {
                Poll::Pending => i += 1,
                Poll::Ready(result) => {
                    busy = true;
                    let h = self.handshakes.swap_remove(i);
                    self.connected(h, result);
                }
            }
        }

        let ids: Vec<u64> = self.conns.keys().copied().collect();
        for id in ids {
            for _ in 0..BATCH {
                let Some(c) = self.conns.get_mut(&id) else {
                    break;
                };
                match Pin::new(&mut c.incoming).poll_next(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Some(Ok(msg))) => {
                        busy = true;
                        self.state.receive(id, &msg);
                    }
                    Poll::Ready(Some(Err(_)) | None) => {
                        busy = true;
                        self.state.disconnected(id);
                        break;
                    }
                }
            }
            self.deliver();
        }

        let mut dead = Vec::new();
        for (id, c) in &mut self.conns {
            match c.drive(cx) {
                Ok(moved) => busy |= moved,
                Err(err) => {
                    tracing::debug!(id, ?err, "bus: cannot send");
                    dead.push(*id);
                }
            }
        }
        for id in dead {
            self.state.disconnected(id);
            busy = true;
        }
        self.deliver();

        if std::mem::take(&mut self.state.changed) {
            self.publish();
        }
        if busy {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }

    /// Start the handshake with a new client.
    fn begin(&mut self, stream: UnixStream, cred: Cred) {
        let id = self.next_id;
        self.next_id += 1;
        let name = unique_name(id);
        let builder = Builder::unix_stream(stream)
            .server(self.guid.clone())
            .p2p()
            .auth_mechanism(AuthMechanism::External)
            .unique_name(name);
        self.handshakes.push(Handshake {
            id,
            cred,
            building: Box::pin(builder.build()),
        });
    }

    fn connected(&mut self, h: Handshake, result: zbus::Result<Connection>) {
        let conn = match result {
            Ok(c) => c,
            Err(err) => {
                (self.notify)(Event::Note(format!(
                    "{} couldn't connect: {err}",
                    crate::services::describe_pid(h.cred.pid)
                )));
                return;
            }
        };
        let incoming = MessageStream::from(&conn);
        self.conns.insert(
            h.id,
            Conn {
                conn,
                incoming,
                queue: VecDeque::new(),
                queued_bytes: 0,
                sending: None,
            },
        );
        self.state.connected(h.id, h.cred);
    }

    /// Move what the bus decided into the connections' queues, and drop the connections it
    /// let go of.
    fn deliver(&mut self) {
        for line in self.state.notes.drain(..) {
            (self.notify)(Event::Note(line));
        }
        let mut overflowed = Vec::new();
        for out in std::mem::take(&mut self.state.out) {
            match out {
                Out::Send(to, msg) => {
                    let Some(c) = self.conns.get_mut(&to) else {
                        continue;
                    };
                    c.queued_bytes += msg.data().len();
                    c.queue.push_back(msg);
                    if c.queued_bytes > MAX_QUEUED {
                        overflowed.push(to);
                    }
                }
                Out::Drop(id) => {
                    self.conns.remove(&id);
                }
            }
        }
        for id in overflowed {
            if self.conns.remove(&id).is_some() {
                let name = self.state.describe(id);
                self.state.notes.push(format!(
                    "{name} stopped reading its messages; disconnected it"
                ));
                self.state.disconnected(id);
            }
        }
        if !self.state.out.is_empty() || !self.state.notes.is_empty() {
            self.deliver();
        }
    }

    fn publish(&mut self) {
        let list = self.state.peer_infos();
        if let Ok(mut peers) = self.peers.lock() {
            *peers = list;
        }
    }
}

impl Conn {
    /// Send what's queued, one message at a time. Returns whether anything was sent, or
    /// the error that broke the connection.
    fn drive(&mut self, cx: &mut Context) -> zbus::Result<bool> {
        let mut moved = false;
        loop {
            if self.sending.is_none() {
                let Some(msg) = self.queue.pop_front() else {
                    return Ok(moved);
                };
                self.queued_bytes -= msg.data().len();
                let conn = self.conn.clone();
                self.sending = Some(Box::pin(async move { conn.send(&msg).await }));
            }
            let Some(sending) = self.sending.as_mut() else {
                return Ok(moved);
            };
            match sending.as_mut().poll(cx) {
                Poll::Pending => return Ok(moved),
                Poll::Ready(result) => {
                    self.sending = None;
                    moved = true;
                    match result {
                        Ok(()) => {}
                        // File descriptors for a client that didn't ask for them: that
                        // message is lost, the connection isn't.
                        Err(zbus::Error::Unsupported) => {}
                        Err(err) => return Err(err),
                    }
                }
            }
        }
    }
}

// --- the bus: names, rules and routing ---------------------------------------------

fn unique_name(id: u64) -> String {
    format!(":1.{id}")
}

fn id_of(unique: &str) -> Option<u64> {
    unique.strip_prefix(":1.")?.parse().ok()
}

enum Out {
    Send(u64, Message),
    /// Disconnect this client (it broke the protocol).
    Drop(u64),
}

struct Peer {
    name: String,
    cred: Cred,
    program: String,
    /// Said Hello, so it has its name and can talk.
    hello: bool,
    rules: Vec<Rule>,
}

#[derive(Debug, Clone, Copy)]
struct Owner {
    peer: u64,
    allow_replacement: bool,
    do_not_queue: bool,
}

/// An error reply: its name after `org.freedesktop.DBus.Error.`, and its text.
type Failure = (&'static str, String);

/// Everything the bus knows, without the sockets: fed messages, it decides what goes
/// where (`out`) and what to log (`notes`).
struct State {
    guid: String,
    peers: BTreeMap<u64, Peer>,
    /// Each well-known name's owner (front) and queue.
    names: BTreeMap<String, VecDeque<Owner>>,
    out: Vec<Out>,
    notes: Vec<String>,
    /// The peer list changed (for the applet).
    changed: bool,
}

impl State {
    fn new(guid: String) -> Self {
        State {
            guid,
            peers: BTreeMap::new(),
            names: BTreeMap::new(),
            out: Vec::new(),
            notes: Vec::new(),
            changed: false,
        }
    }

    fn connected(&mut self, id: u64, cred: Cred) {
        let program = crate::services::process_name(cred.pid);
        self.peers.insert(
            id,
            Peer {
                name: unique_name(id),
                cred,
                program,
                hello: false,
                rules: Vec::new(),
            },
        );
        self.changed = true;
    }

    /// `:1.4 (PID 123 firefox)`.
    fn describe(&self, id: u64) -> String {
        let Some(p) = self.peers.get(&id) else {
            return unique_name(id);
        };
        match Path::new(&p.program).file_name() {
            Some(program) => format!(
                "{} (PID {} {})",
                p.name,
                p.cred.pid,
                program.to_string_lossy()
            ),
            None => format!("{} (PID {})", p.name, p.cred.pid),
        }
    }

    fn peer_infos(&self) -> Vec<PeerInfo> {
        self.peers
            .iter()
            .map(|(id, p)| PeerInfo {
                name: p.name.clone(),
                pid: p.cred.pid,
                program: p.program.clone(),
                names: self
                    .names
                    .iter()
                    .filter(|(_, q)| q.front().is_some_and(|o| o.peer == *id))
                    .map(|(n, _)| n.clone())
                    .collect(),
            })
            .collect()
    }

    fn disconnected(&mut self, id: u64) {
        if !self.peers.contains_key(&id) {
            return;
        }
        let held: Vec<String> = self
            .names
            .iter()
            .filter(|(_, q)| q.iter().any(|o| o.peer == id))
            .map(|(n, _)| n.clone())
            .collect();
        let who = self.describe(id);
        for name in held {
            self.release(id, &name, false);
        }
        if let Some(peer) = self.peers.remove(&id)
            && peer.hello
        {
            self.notes.push(format!("{who} left"));
            self.owner_signal(&peer.name, &peer.name, "");
        }
        self.out.push(Out::Drop(id));
        self.changed = true;
    }

    /// The client that `name` (unique or well-known) means.
    fn owner(&self, name: &str) -> Option<u64> {
        if name.starts_with(':') {
            id_of(name).filter(|id| self.peers.get(id).is_some_and(|p| p.hello))
        } else {
            self.names.get(name)?.front().map(|o| o.peer)
        }
    }

    fn owner_name(&self, name: &str) -> Option<String> {
        if name == NAME {
            return Some(NAME.into());
        }
        self.owner(name).map(unique_name)
    }

    /// A message from client `from`, as it sent it (without a sender).
    fn receive(&mut self, from: u64, msg: &Message) {
        let Some(peer) = self.peers.get(&from) else {
            return;
        };
        let hello = peer.hello;
        let msg = match stamp(msg, &peer.name) {
            Ok(m) => m,
            Err(err) => {
                tracing::debug!(?err, "bus: unreadable message");
                return;
            }
        };
        let hdr = msg.header();
        let to_bus = hdr.destination().is_some_and(|d| d == NAME);
        if !hello {
            if to_bus && hdr.member().is_some_and(|m| m == "Hello") {
                self.hello(from, &hdr);
            } else {
                // dbus-daemon does the same: Hello comes first.
                self.notes.push(format!(
                    "{} talked before saying Hello; disconnected it",
                    self.describe(from)
                ));
                self.disconnected(from);
            }
            return;
        }
        match hdr.destination() {
            Some(_) if to_bus => {
                if msg.message_type() == Type::MethodCall {
                    let result = self.call(from, &msg);
                    if wants_reply(&msg) {
                        let reply = result.and_then(|r| r.map_err(build_failure));
                        let reply = reply.or_else(|(name, text)| error(&hdr, name, &text));
                        if let Ok(reply) = reply {
                            self.out.push(Out::Send(from, reply));
                        }
                    }
                }
            }
            Some(dest) => match self.owner(dest.as_str()) {
                Some(to) => self.out.push(Out::Send(to, msg.clone())),
                None if wants_reply(&msg) => {
                    let text = format!("The name {dest} was not provided by any .service files");
                    if let Ok(reply) = error(&hdr, "ServiceUnknown", &text) {
                        self.out.push(Out::Send(from, reply));
                    }
                }
                None => {}
            },
            None => self.broadcast(&msg),
        }
    }

    fn hello(&mut self, from: u64, hdr: &Header) {
        let Some(peer) = self.peers.get_mut(&from) else {
            return;
        };
        peer.hello = true;
        let name = peer.name.clone();
        if let Ok(reply) = Message::method_return(hdr).sender(NAME).build(&(&name,)) {
            self.out.push(Out::Send(from, reply));
        }
        self.notes
            .push(format!("{} connected", self.describe(from)));
        self.owner_signal(&name, "", &name);
        self.unicast_signal(from, "NameAcquired", &name);
        self.changed = true;
    }

    /// Send `msg` (with no destination) to every client with a rule that matches it.
    fn broadcast(&mut self, msg: &Message) {
        let hdr = msg.header();
        let sender = hdr.sender().map(|s| s.as_str()).unwrap_or_default();
        let args: OnceCell<Rc<rule::Args>> = OnceCell::new();
        let to: Vec<u64> = self
            .peers
            .iter()
            .filter(|(_, p)| p.hello)
            .filter(|(_, p)| {
                p.rules.iter().any(|r| {
                    r.matches(
                        msg,
                        |want| self.sender_is(want, sender),
                        || args.get_or_init(|| Rc::new(rule::args_of(msg))).clone(),
                    )
                })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in to {
            self.out.push(Out::Send(id, msg.clone()));
        }
    }

    /// Whether a rule's `sender='want'` matches a message from `sender`.
    fn sender_is(&self, want: &str, sender: &str) -> bool {
        want == sender
            || (!want.starts_with(':')
                && want != NAME
                && self.owner(want).map(unique_name).as_deref() == Some(sender))
    }

    fn owner_signal(&mut self, name: &str, old: &str, new: &str) {
        let msg = Message::signal(PATH, NAME, "NameOwnerChanged")
            .sender(NAME)
            .build(&(name, old, new));
        match msg {
            Ok(msg) => self.broadcast(&msg),
            Err(err) => tracing::warn!(?err, "bus: cannot build NameOwnerChanged"),
        }
    }

    /// NameAcquired or NameLost, sent to `to` only.
    fn unicast_signal(&mut self, to: u64, member: &str, name: &str) {
        let msg = Message::signal(PATH, NAME, member)
            .sender(NAME)
            .destination(unique_name(to))
            .build(&(name,));
        if let Ok(msg) = msg {
            self.out.push(Out::Send(to, msg));
        }
    }

    /// Who owns `name` changed from `old` to `new` (either may be none).
    fn owner_changed(&mut self, name: &str, old: Option<u64>, new: Option<u64>) {
        if let Some(old) = old {
            self.unicast_signal(old, "NameLost", name);
        }
        let old_name = old.map(unique_name).unwrap_or_default();
        let new_name = new.map(unique_name).unwrap_or_default();
        self.owner_signal(name, &old_name, &new_name);
        if let Some(new) = new {
            self.unicast_signal(new, "NameAcquired", name);
            self.notes
                .push(format!("{} owns {name}", self.describe(new)));
        }
        self.changed = true;
    }

    fn request_name(&mut self, from: u64, name: &str, flags: u32) -> Result<u32, Failure> {
        check_name(name)?;
        let me = Owner {
            peer: from,
            allow_replacement: flags & ALLOW_REPLACEMENT != 0,
            do_not_queue: flags & DO_NOT_QUEUE != 0,
        };
        let queue = self.names.entry(name.to_string()).or_default();
        let Some(&primary) = queue.front() else {
            queue.push_back(me);
            self.owner_changed(name, None, Some(from));
            return Ok(PRIMARY_OWNER);
        };
        if primary.peer == from {
            queue[0] = me;
            return Ok(ALREADY_OWNER);
        }
        if primary.allow_replacement && flags & REPLACE_EXISTING != 0 {
            queue.pop_front();
            queue.retain(|o| o.peer != from);
            // The old owner waits next in line, unless it asked not to queue.
            if !primary.do_not_queue {
                queue.push_front(primary);
            }
            queue.push_front(me);
            self.owner_changed(name, Some(primary.peer), Some(from));
            return Ok(PRIMARY_OWNER);
        }
        if me.do_not_queue {
            queue.retain(|o| o.peer != from);
            return Ok(EXISTS);
        }
        match queue.iter_mut().find(|o| o.peer == from) {
            Some(o) => *o = me,
            None => queue.push_back(me),
        }
        Ok(IN_QUEUE)
    }

    /// `from` gives up `name` (or leaves its queue). `reply`: answer ReleaseName's codes.
    fn release(&mut self, from: u64, name: &str, reply: bool) -> u32 {
        let Some(queue) = self.names.get_mut(name) else {
            return NON_EXISTENT;
        };
        let Some(pos) = queue.iter().position(|o| o.peer == from) else {
            return NOT_OWNER;
        };
        queue.remove(pos);
        let next = queue.front().map(|o| o.peer);
        if queue.is_empty() {
            self.names.remove(name);
        }
        if pos == 0 {
            if reply {
                self.owner_changed(name, Some(from), next);
            } else {
                // It's gone: no NameLost for it.
                self.owner_signal(
                    name,
                    &unique_name(from),
                    &next.map(unique_name).unwrap_or_default(),
                );
                if let Some(next) = next {
                    self.unicast_signal(next, "NameAcquired", name);
                    self.notes
                        .push(format!("{} owns {name}", self.describe(next)));
                }
                self.changed = true;
            }
        }
        RELEASED
    }

    /// A method call to the bus. The inner error is from building the reply.
    fn call(&mut self, from: u64, msg: &Message) -> Result<zbus::Result<Message>, Failure> {
        let hdr = msg.header();
        let body = msg.body();
        let member = hdr.member().map(|m| m.as_str()).unwrap_or_default();
        let iface = hdr.interface().map(|i| i.as_str());
        let ok = |reply: &dyn ReplyBody| reply.build(&hdr);
        macro_rules! args {
            ($t:ty) => {
                body.deserialize::<$t>()
                    .map_err(|e| ("InvalidArgs", format!("{member}: {e}")))?
            };
        }
        let iface = match iface {
            None => match member {
                "Ping" | "GetMachineId" => "org.freedesktop.DBus.Peer",
                "Introspect" => "org.freedesktop.DBus.Introspectable",
                _ => NAME,
            },
            Some(i) => i,
        };
        Ok(match (iface, member) {
            (NAME, "Hello") => {
                return Err(("Failed", "Already handled an Hello message".into()));
            }
            (NAME, "RequestName") => {
                let (name, flags) = args!((String, u32));
                ok(&(self.request_name(from, &name, flags)?,))
            }
            (NAME, "ReleaseName") => {
                let (name,) = args!((String,));
                check_name(&name)?;
                ok(&(self.release(from, &name, true),))
            }
            (NAME, "ListQueuedOwners") => {
                let (name,) = args!((String,));
                let owners: Vec<String> = match self.names.get(&name) {
                    Some(q) => q.iter().map(|o| unique_name(o.peer)).collect(),
                    None if name == NAME => vec![NAME.into()],
                    None => return Err(no_owner("owners", &name)),
                };
                ok(&(owners,))
            }
            (NAME, "ListNames") => {
                let mut names = vec![NAME.to_string()];
                names.extend(
                    self.peers
                        .values()
                        .filter(|p| p.hello)
                        .map(|p| p.name.clone()),
                );
                names.extend(self.names.keys().cloned());
                ok(&(names,))
            }
            (NAME, "ListActivatableNames") => ok(&(vec![NAME],)),
            (NAME, "NameHasOwner") => {
                let (name,) = args!((String,));
                ok(&(self.owner_name(&name).is_some(),))
            }
            (NAME, "GetNameOwner") => {
                let (name,) = args!((String,));
                let owner = self
                    .owner_name(&name)
                    .ok_or_else(|| no_owner("owner", &name))?;
                ok(&(owner,))
            }
            (NAME, "StartServiceByName") => {
                let (name, _flags) = args!((String, u32));
                if self.owner_name(&name).is_none() {
                    return Err((
                        "ServiceUnknown",
                        format!("The name {name} was not provided by any .service files"),
                    ));
                }
                ok(&(ALREADY_RUNNING,))
            }
            // Nothing is started on demand, so there's no environment to update.
            (NAME, "UpdateActivationEnvironment") => {
                let _ = args!((HashMap<String, String>,));
                ok(&())
            }
            (NAME, "AddMatch") => {
                let (text,) = args!((String,));
                let rule = Rule::parse(&text).map_err(|e| ("MatchRuleInvalid", e))?;
                let peer = self.peers.get_mut(&from).expect("sender is connected");
                if peer.rules.len() >= MAX_RULES {
                    return Err(("LimitsExceeded", "too many match rules".into()));
                }
                peer.rules.push(rule);
                ok(&())
            }
            (NAME, "RemoveMatch") => {
                let (text,) = args!((String,));
                let rule = Rule::parse(&text).map_err(|e| ("MatchRuleInvalid", e))?;
                let peer = self.peers.get_mut(&from).expect("sender is connected");
                let pos = peer.rules.iter().position(|r| *r == rule).ok_or((
                    "MatchRuleNotFound",
                    "The given match rule wasn't found".into(),
                ))?;
                peer.rules.remove(pos);
                ok(&())
            }
            (NAME, "GetConnectionUnixUser") => {
                let (name,) = args!((String,));
                ok(&(self.cred(&name, "UID")?.uid,))
            }
            (NAME, "GetConnectionUnixProcessID") => {
                let (name,) = args!((String,));
                ok(&(self.cred(&name, "PID")?.pid,))
            }
            (NAME, "GetConnectionCredentials") => {
                let (name,) = args!((String,));
                let cred = self.cred(&name, "credentials")?;
                let mut map: HashMap<&str, Value> = HashMap::new();
                map.insert("UnixUserID", Value::from(cred.uid));
                map.insert("ProcessID", Value::from(cred.pid));
                map.insert("UnixGroupIDs", Value::from(vec![cred.gid]));
                ok(&(map,))
            }
            (NAME, "GetAdtAuditSessionData") => {
                return Err(("AdtAuditDataUnknown", "No audit data here".into()));
            }
            (NAME, "GetConnectionSELinuxSecurityContext") => {
                return Err((
                    "SELinuxSecurityContextUnknown",
                    "No SELinux context here".into(),
                ));
            }
            (NAME, "GetId") => ok(&(&self.guid,)),
            (NAME, "ReloadConfig") => ok(&()),
            ("org.freedesktop.DBus.Properties", "Get") => {
                let (_iface, prop) = args!((String, String));
                match prop.as_str() {
                    "Features" | "Interfaces" => ok(&(Value::from(Vec::<String>::new()),)),
                    _ => return Err(("UnknownProperty", format!("No property {prop}"))),
                }
            }
            ("org.freedesktop.DBus.Properties", "GetAll") => {
                let mut map: HashMap<&str, Value> = HashMap::new();
                map.insert("Features", Value::from(Vec::<String>::new()));
                map.insert("Interfaces", Value::from(Vec::<String>::new()));
                ok(&(map,))
            }
            ("org.freedesktop.DBus.Properties", "Set") => {
                return Err((
                    "PropertyReadOnly",
                    "The bus's properties are read-only".into(),
                ));
            }
            ("org.freedesktop.DBus.Peer", "Ping") => ok(&()),
            ("org.freedesktop.DBus.Peer", "GetMachineId") => {
                let id = std::fs::read_to_string("/etc/machine-id")
                    .map(|s| s.trim().to_string())
                    .map_err(|e| ("Failed", format!("cannot read /etc/machine-id: {e}")))?;
                ok(&(id,))
            }
            ("org.freedesktop.DBus.Introspectable", "Introspect") => ok(&(INTROSPECTION,)),
            ("org.freedesktop.DBus.Monitoring", "BecomeMonitor") => {
                return Err(("NotSupported", "simplewm's bus has no monitors".into()));
            }
            (iface, member) => {
                return Err((
                    "UnknownMethod",
                    format!("{iface} has no method {member} on the bus"),
                ));
            }
        })
    }

    /// The credentials of whoever `name` means (`what` is for the error message).
    fn cred(&self, name: &str, what: &str) -> Result<Cred, Failure> {
        if name == NAME {
            // SAFETY: getuid and getgid can't fail.
            return Ok(Cred {
                pid: std::process::id(),
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
            });
        }
        self.owner(name)
            .and_then(|id| self.peers.get(&id))
            .map(|p| p.cred)
            .ok_or_else(|| no_owner(what, name))
    }
}

/// A reply body: anything zbus can serialize, behind one object-safe call.
trait ReplyBody {
    fn build(&self, call: &Header) -> zbus::Result<Message>;
}

impl<T: zbus::export::serde::Serialize + DynamicType> ReplyBody for T {
    fn build(&self, call: &Header) -> zbus::Result<Message> {
        Message::method_return(call).sender(NAME).build(self)
    }
}

fn build_failure(err: zbus::Error) -> Failure {
    ("Failed", format!("cannot build the reply: {err}"))
}

fn error(call: &Header, name: &str, text: &str) -> zbus::Result<Message> {
    Message::error(call, format!("{ERR}{name}"))
        .sender(NAME)
        .build(&(text,))
}

fn no_owner(what: &str, name: &str) -> Failure {
    (
        "NameHasNoOwner",
        format!("Could not get {what} of name '{name}': no such name"),
    )
}

/// A name a client may own.
fn check_name(name: &str) -> Result<(), Failure> {
    if name == NAME {
        return Err(("InvalidArgs", format!("{NAME} belongs to the bus itself")));
    }
    if name.starts_with(':') {
        return Err((
            "InvalidArgs",
            format!("Cannot acquire a name starting with ':' such as \"{name}\""),
        ));
    }
    WellKnownName::try_from(name)
        .map(|_| ())
        .map_err(|_| ("InvalidArgs", format!("\"{name}\" is not a valid bus name")))
}

fn wants_reply(msg: &Message) -> bool {
    msg.message_type() == Type::MethodCall
        && !msg
            .primary_header()
            .flags()
            .contains(Flags::NO_REPLY_EXPECTED)
}

/// `msg` with its sender set to `sender` (whatever the client put there).
fn stamp(msg: &Message, sender: &str) -> zbus::Result<Message> {
    let hdr = msg.header();
    let body = msg.body();
    let data = body.data();
    let fds = data
        .fds()
        .iter()
        .map(|fd| {
            use std::os::fd::AsFd;
            fd.as_fd()
                .try_clone_to_owned()
                .map(zbus::OwnedFd::from)
                .map_err(|e| zbus::Error::InputOutput(Arc::new(e)))
        })
        .collect::<zbus::Result<Vec<_>>>()?;
    let signature = hdr.signature().clone();
    let builder = zbus::message::Builder::from(hdr).sender(sender);
    // SAFETY: the body, its signature and its file descriptors come from one valid
    // message, unchanged.
    unsafe { builder.build_raw_body(data.bytes(), signature, fds) }
}

const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.freedesktop.DBus">
    <method name="Hello"><arg direction="out" type="s"/></method>
    <method name="RequestName"><arg direction="in" type="s"/><arg direction="in" type="u"/><arg direction="out" type="u"/></method>
    <method name="ReleaseName"><arg direction="in" type="s"/><arg direction="out" type="u"/></method>
    <method name="StartServiceByName"><arg direction="in" type="s"/><arg direction="in" type="u"/><arg direction="out" type="u"/></method>
    <method name="UpdateActivationEnvironment"><arg direction="in" type="a{ss}"/></method>
    <method name="NameHasOwner"><arg direction="in" type="s"/><arg direction="out" type="b"/></method>
    <method name="ListNames"><arg direction="out" type="as"/></method>
    <method name="ListActivatableNames"><arg direction="out" type="as"/></method>
    <method name="AddMatch"><arg direction="in" type="s"/></method>
    <method name="RemoveMatch"><arg direction="in" type="s"/></method>
    <method name="GetNameOwner"><arg direction="in" type="s"/><arg direction="out" type="s"/></method>
    <method name="ListQueuedOwners"><arg direction="in" type="s"/><arg direction="out" type="as"/></method>
    <method name="GetConnectionUnixUser"><arg direction="in" type="s"/><arg direction="out" type="u"/></method>
    <method name="GetConnectionUnixProcessID"><arg direction="in" type="s"/><arg direction="out" type="u"/></method>
    <method name="GetConnectionCredentials"><arg direction="in" type="s"/><arg direction="out" type="a{sv}"/></method>
    <method name="GetAdtAuditSessionData"><arg direction="in" type="s"/><arg direction="out" type="ay"/></method>
    <method name="GetConnectionSELinuxSecurityContext"><arg direction="in" type="s"/><arg direction="out" type="ay"/></method>
    <method name="ReloadConfig"/>
    <method name="GetId"><arg direction="out" type="s"/></method>
    <signal name="NameOwnerChanged"><arg type="s"/><arg type="s"/><arg type="s"/></signal>
    <signal name="NameLost"><arg type="s"/></signal>
    <signal name="NameAcquired"><arg type="s"/></signal>
    <property name="Features" type="as" access="read"/>
    <property name="Interfaces" type="as" access="read"/>
  </interface>
  <interface name="org.freedesktop.DBus.Properties">
    <method name="Get"><arg direction="in" type="s"/><arg direction="in" type="s"/><arg direction="out" type="v"/></method>
    <method name="GetAll"><arg direction="in" type="s"/><arg direction="out" type="a{sv}"/></method>
    <method name="Set"><arg direction="in" type="s"/><arg direction="in" type="s"/><arg direction="in" type="v"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg direction="out" type="s"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Peer">
    <method name="Ping"/>
    <method name="GetMachineId"><arg direction="out" type="s"/></method>
  </interface>
</node>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Read, sync::mpsc as std_mpsc, time::Duration};
    use zbus::fdo::{DBusProxy, ReleaseNameReply, RequestNameFlags, RequestNameReply};

    /// A bus on a scratch socket, and its log.
    fn scratch_bus(name: &str) -> (Bus, String, Arc<Mutex<Vec<String>>>) {
        let dir = std::env::temp_dir().join(format!("simplewm-bus-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus");
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        let bus = Bus::start(
            &path,
            Arc::new(move |ev| {
                let line = match ev {
                    Event::Note(l) => l,
                    Event::Stopped(how) => format!("STOPPED {how}"),
                };
                sink.lock().unwrap().push(line);
            }),
        )
        .unwrap();
        (bus, format!("unix:path={}", path.display()), log)
    }

    async fn client(address: &str) -> Connection {
        zbus::connection::Builder::address(address)
            .build()
            .await
            .unwrap()
    }

    /// Run `f` on its own thread, failing the test if it takes over 20 seconds.
    fn within(f: impl FnOnce() + Send + 'static) {
        let (tx, rx) = std_mpsc::channel();
        let t = std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(()) => t.join().unwrap(),
            Err(_) if t.is_finished() => std::panic::resume_unwind(t.join().unwrap_err()),
            Err(_) => panic!("timed out"),
        }
    }

    async fn add_match(conn: &Connection, rule: &str) {
        conn.call_method(Some(NAME), PATH, Some(NAME), "AddMatch", &(rule,))
            .await
            .unwrap();
    }

    struct Test;

    #[zbus::interface(name = "org.simplewm.Test")]
    impl Test {
        fn echo(&self, s: String) -> String {
            s
        }

        /// Write "ok" into the file descriptor it's given.
        fn write(&self, fd: zbus::OwnedFd) -> zbus::fdo::Result<()> {
            use std::io::Write;
            let fd: std::os::fd::OwnedFd = fd.into();
            std::fs::File::from(fd)
                .write_all(b"ok")
                .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
        }
    }

    /// The next signal on `stream` from interface `iface`, as (member, first argument).
    async fn next_signal(stream: &mut MessageStream, iface: &str) -> (String, String) {
        loop {
            let msg = std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx))
                .await
                .unwrap()
                .unwrap();
            let hdr = msg.header();
            if msg.message_type() == Type::Signal && hdr.interface().is_some_and(|i| i == iface) {
                let member = hdr.member().unwrap().to_string();
                let arg = msg
                    .body()
                    .deserialize::<zbus::Structure>()
                    .ok()
                    .and_then(|s| {
                        s.fields()
                            .first()
                            .and_then(|v| String::try_from(v.clone()).ok())
                    })
                    .unwrap_or_default();
                return (member, arg);
            }
        }
    }

    #[test]
    fn routes_calls_and_names() {
        let (bus, address, log) = scratch_bus("routes");
        within(move || {
            zbus::block_on(async move {
                let a = client(&address).await;
                let b = client(&address).await;
                let a_name = a.unique_name().unwrap().to_string();
                assert!(a_name.starts_with(":1."));
                a.object_server().at("/test", Test).await.unwrap();
                let reply = a
                    .request_name_with_flags("org.simplewm.Test", RequestNameFlags::empty())
                    .await
                    .unwrap();
                assert_eq!(reply, RequestNameReply::PrimaryOwner);

                let dbus = DBusProxy::new(&b).await.unwrap();
                let owner = dbus
                    .get_name_owner("org.simplewm.Test".try_into().unwrap())
                    .await
                    .unwrap();
                assert_eq!(owner.as_str(), a_name);
                let names = dbus.list_names().await.unwrap();
                assert!(names.iter().any(|n| n.as_str() == "org.simplewm.Test"));
                assert!(names.iter().any(|n| n.as_str() == NAME));
                let pid = dbus
                    .get_connection_unix_process_id("org.simplewm.Test".try_into().unwrap())
                    .await
                    .unwrap();
                assert_eq!(pid, std::process::id());

                // A call through the well-known name, and one with a file descriptor.
                let reply = b
                    .call_method(
                        Some("org.simplewm.Test"),
                        "/test",
                        Some("org.simplewm.Test"),
                        "Echo",
                        &("hi",),
                    )
                    .await
                    .unwrap();
                assert_eq!(reply.body().deserialize::<String>().unwrap(), "hi");
                let (mut read, write) = std::io::pipe().unwrap();
                let write: std::os::fd::OwnedFd = write.into();
                b.call_method(
                    Some(a_name.as_str()),
                    "/test",
                    Some("org.simplewm.Test"),
                    "Write",
                    &(zbus::Fd::from(&write),),
                )
                .await
                .unwrap();
                drop(write);
                let mut got = String::new();
                read.read_to_string(&mut got).unwrap();
                assert_eq!(got, "ok");

                // Nobody owns this.
                let err = b
                    .call_method(Some("org.simplewm.Nobody"), "/", None::<&str>, "X", &())
                    .await
                    .unwrap_err();
                assert!(err.to_string().contains("ServiceUnknown"), "{err}");

                // Errors from the bus itself.
                let err = dbus
                    .get_name_owner("org.simplewm.Nobody".try_into().unwrap())
                    .await
                    .unwrap_err();
                assert!(err.to_string().contains("NameHasNoOwner"), "{err}");
                let err = b.request_name("org.freedesktop.DBus").await.unwrap_err();
                assert!(err.to_string().contains("InvalidArgs"), "{err}");
                let err = b
                    .call_method(Some(NAME), PATH, Some(NAME), "AddMatch", &("colour='red'",))
                    .await
                    .unwrap_err();
                assert!(err.to_string().contains("MatchRuleInvalid"), "{err}");
                let id = dbus.get_id().await.unwrap();
                assert_eq!(id.as_str().len(), 32);
            });
        });
        assert!(bus.peers().is_empty() || bus.peers().len() <= 2);
        let log = log.lock().unwrap().clone();
        assert!(log.iter().any(|l| l.contains("connected")), "{log:?}");
        assert!(
            log.iter().any(|l| l.contains("owns org.simplewm.Test")),
            "{log:?}"
        );
    }

    #[test]
    fn queues_and_hands_over_names() {
        let (_bus, address, _log) = scratch_bus("names");
        within(move || {
            zbus::block_on(async move {
                let watcher = client(&address).await;
                let dbus = DBusProxy::new(&watcher).await.unwrap();
                let mut changes = MessageStream::from(&watcher);
                add_match(
                    &watcher,
                    "type='signal',sender='org.freedesktop.DBus',member='NameOwnerChanged',\
                     arg0='org.simplewm.Name'",
                )
                .await;
                let owner = |c: &Connection| c.unique_name().unwrap().to_string();
                let a = client(&address).await;
                let b = client(&address).await;
                // Straight to the bus: zbus's own request_name remembers answers.
                let (bus_a, bus_b) = (
                    DBusProxy::new(&a).await.unwrap(),
                    DBusProxy::new(&b).await.unwrap(),
                );
                let name = || "org.simplewm.Name".try_into().unwrap();
                let flags = RequestNameFlags::ALLOW_REPLACEMENT;
                assert_eq!(
                    bus_a.request_name(name(), flags).await.unwrap(),
                    RequestNameReply::PrimaryOwner
                );
                assert_eq!(
                    bus_a.request_name(name(), flags).await.unwrap(),
                    RequestNameReply::AlreadyOwner
                );
                let flags = RequestNameFlags::DO_NOT_QUEUE;
                assert_eq!(
                    bus_b.request_name(name(), flags).await.unwrap(),
                    RequestNameReply::Exists
                );
                let flags = RequestNameFlags::empty();
                assert_eq!(
                    bus_b.request_name(name(), flags).await.unwrap(),
                    RequestNameReply::InQueue
                );
                let queued = dbus.list_queued_owners(name()).await.unwrap();
                assert_eq!(queued.len(), 2);

                // Replacing: B takes over, A waits next in line.
                let flags = RequestNameFlags::REPLACE_EXISTING;
                assert_eq!(
                    bus_b.request_name(name(), flags).await.unwrap(),
                    RequestNameReply::PrimaryOwner
                );
                let get_owner = || async {
                    dbus.get_name_owner("org.simplewm.Name".try_into().unwrap())
                        .await
                        .map(|o| o.to_string())
                        .ok()
                };
                assert_eq!(get_owner().await, Some(owner(&b)));

                // B lets go: A has it again. A leaves: nobody has it.
                assert_eq!(
                    bus_b.release_name(name()).await.unwrap(),
                    ReleaseNameReply::Released
                );
                assert_eq!(
                    bus_b.release_name(name()).await.unwrap(),
                    ReleaseNameReply::NotOwner
                );
                assert_eq!(get_owner().await, Some(owner(&a)));
                drop(bus_a);
                let a_name = owner(&a);
                drop(a);
                // Its name goes when the bus notices it's gone.
                let mut gone = false;
                for _ in 0..200 {
                    if get_owner().await.is_none() {
                        gone = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert!(gone);
                let none = dbus
                    .release_name("org.simplewm.Name".try_into().unwrap())
                    .await
                    .unwrap();
                assert_eq!(none, ReleaseNameReply::NonExistent);

                // The watcher saw every change, in order.
                let mut seen = Vec::new();
                while seen.len() < 4 {
                    let msg = std::future::poll_fn(|cx| Pin::new(&mut changes).poll_next(cx))
                        .await
                        .unwrap()
                        .unwrap();
                    if msg
                        .header()
                        .member()
                        .is_some_and(|m| m == "NameOwnerChanged")
                    {
                        let (_, old, new): (String, String, String) =
                            msg.body().deserialize().unwrap();
                        seen.push((old, new));
                    }
                }
                let b_name = owner(&b);
                let none = String::new();
                assert_eq!(
                    seen,
                    [
                        (none.clone(), a_name.clone()),
                        (a_name.clone(), b_name.clone()),
                        (b_name, a_name.clone()),
                        (a_name, none),
                    ]
                );
            });
        });
    }

    #[test]
    fn delivers_signals_by_rule() {
        let (_bus, address, _log) = scratch_bus("signals");
        within(move || {
            zbus::block_on(async move {
                let a = client(&address).await;
                let b = client(&address).await;
                let mut stream = MessageStream::from(&a);
                add_match(&a, "type='signal',interface='org.simplewm.Sig',arg0='yes'").await;
                for arg in ["no", "yes"] {
                    b.emit_signal(None::<&str>, "/x", "org.simplewm.Sig", "Ping", &(arg,))
                        .await
                        .unwrap();
                }
                assert_eq!(
                    next_signal(&mut stream, "org.simplewm.Sig").await,
                    ("Ping".into(), "yes".into())
                );
            });
        });
    }

    #[test]
    fn stopping_disconnects_everyone() {
        let (bus, address, log) = scratch_bus("stop");
        let path = address.trim_start_matches("unix:path=").to_string();
        within(move || {
            zbus::block_on(async move {
                let a = client(&address).await;
                let dbus = DBusProxy::new(&a).await.unwrap();
                dbus.list_names().await.unwrap();
                bus.stop();
                assert!(dbus.list_names().await.is_err());
            });
        });
        assert!(!Path::new(&path).exists());
        assert!(!log.lock().unwrap().iter().any(|l| l.starts_with("STOPPED")));
    }

    /// Real C clients: libdbus (`dbus-send`) and GLib (`gdbus`) against a scratch bus.
    /// Needs both: `cargo test talks_to_c_clients -- --ignored`.
    #[test]
    #[ignore]
    fn talks_to_c_clients() {
        let (_bus, address, _log) = scratch_bus("c-clients");
        // On its own thread, so this one keeps serving `a` meanwhile.
        let run = {
            let address = address.clone();
            move |args: &[&str]| {
                let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                let address = address.clone();
                let (tx, rx) = futures_channel::oneshot::channel();
                std::thread::spawn(move || {
                    let out = std::process::Command::new(&args[0])
                        .args(&args[1..])
                        .env("DBUS_SESSION_BUS_ADDRESS", &address)
                        .output()
                        .unwrap();
                    let text = String::from_utf8_lossy(&out.stdout).into_owned();
                    let err = String::from_utf8_lossy(&out.stderr).into_owned();
                    let _ = tx.send((out.status.success(), format!("{args:?}: {err}{text}"), text));
                });
                async move {
                    let (ok, why, text) = rx.await.unwrap();
                    assert!(ok, "{why}");
                    text
                }
            }
        };
        within(move || {
            zbus::block_on(async move {
                let a = client(&address).await;
                a.object_server().at("/test", Test).await.unwrap();
                a.request_name("org.simplewm.Test").await.unwrap();
                let names = run(&[
                    "dbus-send",
                    "--session",
                    "--print-reply",
                    "--dest=org.freedesktop.DBus",
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus.ListNames",
                ])
                .await;
                assert!(names.contains("org.simplewm.Test"), "{names}");
                let names = run(&[
                    "gdbus",
                    "call",
                    "--session",
                    "--dest",
                    "org.freedesktop.DBus",
                    "--object-path",
                    "/org/freedesktop/DBus",
                    "--method",
                    "org.freedesktop.DBus.ListNames",
                ])
                .await;
                assert!(names.contains("org.simplewm.Test"), "{names}");
                let xml = run(&[
                    "gdbus",
                    "introspect",
                    "--session",
                    "--dest",
                    "org.freedesktop.DBus",
                    "--object-path",
                    "/org/freedesktop/DBus",
                ])
                .await;
                assert!(xml.contains("RequestName"), "{xml}");
                let echo = run(&[
                    "gdbus",
                    "call",
                    "--session",
                    "--dest",
                    "org.simplewm.Test",
                    "--object-path",
                    "/test",
                    "--method",
                    "org.simplewm.Test.Echo",
                    "hi",
                ])
                .await;
                assert_eq!(echo.trim(), "('hi',)");
                let echo = run(&[
                    "dbus-send",
                    "--session",
                    "--print-reply",
                    "--dest=org.simplewm.Test",
                    "/test",
                    "org.simplewm.Test.Echo",
                    "string:there",
                ])
                .await;
                assert!(echo.contains("\"there\""), "{echo}");

                let mut stream = MessageStream::from(&a);
                add_match(&a, "type='signal',interface='org.simplewm.Sig'").await;
                run(&[
                    "gdbus",
                    "emit",
                    "--session",
                    "--object-path",
                    "/x",
                    "--signal",
                    "org.simplewm.Sig.Ping",
                    "'from glib'",
                ])
                .await;
                assert_eq!(
                    next_signal(&mut stream, "org.simplewm.Sig").await,
                    ("Ping".into(), "from glib".into())
                );
                run(&[
                    "dbus-send",
                    "--session",
                    "/x",
                    "org.simplewm.Sig.Pong",
                    "string:from libdbus",
                ])
                .await;
                assert_eq!(
                    next_signal(&mut stream, "org.simplewm.Sig").await,
                    ("Pong".into(), "from libdbus".into())
                );
            });
        });
    }
}
