//! simplewm as the desktop portal (`org.freedesktop.portal.Desktop`) for file dialogs.
//!
//! Apps ask the portal for a file dialog on `org.freedesktop.portal.FileChooser`. simplewm
//! owns that bus name itself, so no `xdg-desktop-portal` process is involved. Only
//! FileChooser is implemented (no `SaveFiles`); apps get "unknown interface" for anything
//! else (Settings, OpenURI, …) and fall back to their defaults. Files are returned as plain
//! paths, so sandboxed (Flatpak) apps can't open them: that needs the document portal.
//!
//! A call returns a request handle straight away; the answer goes out later as the
//! `Response` signal on that handle's `org.freedesktop.portal.Request` object, sent to the
//! caller only. Apps work out the handle in advance from their unique bus name and the
//! `handle_token` option, and subscribe to it before calling, so `request_path` must match.
//!
//! D-Bus runs on a "portal" thread that sits in `zbus::block_on` for good, which is what
//! drives the connection. Requests reach the compositor through a calloop channel; each
//! carries a oneshot sender for the answer. `serve` waits on those answers and on the
//! bus's NameOwnerChanged signal (to cancel the dialogs of apps that quit) in one loop.
//!
//! Only started on a TTY: nested, the host session's portal keeps the name.

use std::{
    collections::HashMap,
    future::Future,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Poll, Waker},
};

use futures_channel::oneshot;
use smithay::reexports::calloop::channel::Sender;
use zbus::{
    Connection, ObjectServer, OwnedObjectPath, OwnedVariant, Variant,
    export::futures_core::Stream,
    fdo::{self, DBusProxy, RequestNameFlags, RequestNameReply},
    interface,
    message::Header,
    names::{BusName, OwnedUniqueName},
    object_server::SignalEmitter,
};

pub const BUS_NAME: &str = "org.freedesktop.portal.Desktop";
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
const REQUEST_PATH: &str = "/org/freedesktop/portal/desktop/request";
/// The FileChooser interface version we implement (4 added `current_folder` to OpenFile).
pub const VERSION: u32 = 4;

/// Portal response codes.
const RESPONSE_OK: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;
const RESPONSE_OTHER: u32 = 2;

/// Sent from the D-Bus thread to the compositor.
pub enum PortalEvent {
    Pick(PickRequest),
    /// Request `.0` was closed by the app, or the app went away.
    Close(u64),
}

/// Where the answer to a File Picker request goes.
pub enum PickReply {
    /// An app's request over the portal.
    Portal(oneshot::Sender<PickResult>),
    /// The Screenshot applet's Choose button: set the screenshot folder.
    ScreenshotDir,
    /// The Launcher applet's Add button: add the chosen executables.
    LauncherAdd,
    /// The Launcher applet's Change button: replace this entry.
    LauncherChange(String),
}

/// A file dialog request.
pub struct PickRequest {
    pub id: u64,
    pub save: bool,
    pub multiple: bool,
    pub directory: bool,
    pub title: String,
    pub accept_label: Option<String>,
    /// Folder to start in.
    pub folder: Option<PathBuf>,
    /// Suggested file name (Save).
    pub name: String,
    pub filters: Vec<Filter>,
    /// Index into `filters` selected initially.
    pub current_filter: Option<usize>,
    pub reply: PickReply,
}

#[derive(Debug, Clone)]
pub struct Filter {
    pub name: String,
    /// Lower-case glob patterns (mime types are translated to globs).
    pub globs: Vec<String>,
}

impl Filter {
    pub fn matches(&self, file_name: &str) -> bool {
        let name = file_name.to_lowercase();
        self.globs
            .iter()
            .any(|g| glob_match(g.as_bytes(), name.as_bytes()))
    }
}

pub enum PickResult {
    Chosen(Vec<PathBuf>),
    Cancelled,
    Failed,
}

/// Whether simplewm is the portal, shown in the Desktop Portal applet.
#[derive(Clone, Default)]
pub enum State {
    /// Not started: nested, the host session's portal answers.
    #[default]
    Off,
    Starting,
    /// We own `BUS_NAME` and serve requests.
    Serving,
    /// Another process owns `BUS_NAME`; we get it when that one lets go.
    Waiting {
        owner: Option<u32>,
    },
    /// Gave up, with why.
    Failed(String),
}

/// How the portal is doing, shown in the Desktop Portal applet.
#[derive(Clone, Default)]
pub struct Status {
    pub state: State,
    /// File dialog requests received since startup.
    pub requests: u64,
    /// The most recent request and how it ended.
    pub last_request: Option<String>,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    state: State::Off,
    requests: 0,
    last_request: None,
});

pub fn status() -> Status {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn update_status(f: impl FnOnce(&mut Status)) {
    if let Ok(mut s) = STATUS.lock() {
        f(&mut s);
    }
}

fn set_state(state: State) {
    update_status(|s| s.state = state);
}

/// Become the desktop portal on the session bus (in the background).
pub fn start(tx: Sender<PortalEvent>) {
    set_state(State::Starting);
    let spawned = std::thread::Builder::new()
        .name("portal".into())
        .spawn(move || {
            let shared = Arc::new(Shared::default());
            let chooser = FileChooser {
                tx: tx.clone(),
                next_id: AtomicU64::new(1),
                shared: shared.clone(),
            };
            // The connection only runs while this thread is inside block_on, so serve()
            // never returns while the portal is up.
            zbus::block_on(serve(chooser, tx, shared));
        });
    if let Err(err) = spawned {
        tracing::warn!(?err, "portal: cannot start thread");
        set_state(State::Failed(format!(
            "cannot start the D-Bus thread: {err}"
        )));
    }
}

/// Requests waiting for an answer, shared by the D-Bus methods and `serve`.
#[derive(Default)]
struct Shared {
    pending: Mutex<Vec<Pending>>,
    /// Wakes `serve` when a request is added.
    waker: Mutex<Option<Waker>>,
}

struct Pending {
    id: u64,
    /// The app's unique bus name.
    sender: OwnedUniqueName,
    handle: OwnedObjectPath,
    /// Closed by the app (or it quit): answer the compositor, but send no Response.
    closed: Arc<AtomicBool>,
    answer: oneshot::Receiver<PickResult>,
}

/// Connect, take the bus name and serve requests forever (or return on failure).
async fn serve(chooser: FileChooser, tx: Sender<PortalEvent>, shared: Arc<Shared>) {
    let fail = |what: &str, err: zbus::Error| {
        tracing::warn!(?err, "portal: {what}");
        set_state(State::Failed(format!("{what}: {err}")));
    };
    let conn = zbus::connection::Builder::session()
        .serve_at(OBJECT_PATH, chooser)
        .build()
        .await;
    let conn = match conn {
        Ok(conn) => conn,
        Err(err) => return fail("cannot connect to the session bus", err),
    };
    let dbus = match DBusProxy::new(&conn).await {
        Ok(d) => d,
        Err(err) => return fail("cannot talk to the bus", err),
    };
    // Subscribe before asking for the name, so the hand-over can't be missed.
    let owners = match dbus.receive_name_owner_changed().await {
        Ok(s) => s,
        Err(err) => return fail("cannot watch bus names", err),
    };
    let mut owners = Box::pin(owners);
    let me = conn
        .unique_name()
        .map(|n| n.to_string())
        .unwrap_or_default();

    // Queue for the name if someone else has it (no replacing, no being replaced).
    match conn
        .request_name_with_flags(BUS_NAME, RequestNameFlags::empty())
        .await
    {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => serving(),
        Ok(RequestNameReply::InQueue | RequestNameReply::Exists) => {
            let owner = owner_pid(&dbus).await;
            tracing::info!(?owner, "portal: another process has the name; waiting");
            set_state(State::Waiting { owner });
            // A leftover xdg-desktop-portal: on a TTY we are the session, so stop it.
            // (It can't come back: D-Bus only starts it while nobody owns the name.)
            if owner.is_some_and(|pid| process_name(pid).contains("xdg-desktop-portal")) {
                stop_xdg_desktop_portal();
            }
        }
        Err(err) => return fail("cannot ask for the bus name", err),
    }

    loop {
        let (changes, finished, ended) = std::future::poll_fn(|cx| {
            if let Ok(mut w) = shared.waker.lock() {
                *w = Some(cx.waker().clone());
            }
            let mut changes = Vec::new();
            let mut ended = false;
            loop {
                match owners.as_mut().poll_next(cx) {
                    Poll::Ready(Some(change)) => changes.push(change),
                    Poll::Ready(None) => {
                        ended = true;
                        break;
                    }
                    Poll::Pending => break,
                }
            }
            let mut finished = Vec::new();
            if let Ok(mut pending) = shared.pending.lock() {
                let mut i = 0;
                while i < pending.len() {
                    match Pin::new(&mut pending[i].answer).poll(cx) {
                        Poll::Ready(result) => {
                            let p = pending.swap_remove(i);
                            finished.push((p, result.unwrap_or(PickResult::Failed)));
                        }
                        Poll::Pending => i += 1,
                    }
                }
            }
            if changes.is_empty() && finished.is_empty() && !ended {
                Poll::Pending
            } else {
                Poll::Ready((changes, finished, ended))
            }
        })
        .await;

        for change in changes {
            let Ok(args) = change.args() else { continue };
            let new_owner = args.new_owner().as_ref().map(|n| n.as_str());
            if args.name().as_str() == BUS_NAME {
                if new_owner == Some(me.as_str()) {
                    serving();
                } else if new_owner.is_some() {
                    set_state(State::Waiting {
                        owner: owner_pid(&dbus).await,
                    });
                }
            } else if new_owner.is_none() {
                // An app left the bus: cancel its dialogs.
                close_requests_from(&shared, &tx, args.name().as_str());
            }
        }
        for (p, result) in finished {
            finish(&conn, p, result).await;
        }
        if ended {
            set_state(State::Failed("lost the session bus".into()));
            return;
        }
    }
}

fn serving() {
    tracing::info!(name = BUS_NAME, "portal: serving file dialogs");
    set_state(State::Serving);
}

/// Who owns `BUS_NAME`, by PID.
async fn owner_pid(dbus: &DBusProxy<'_>) -> Option<u32> {
    let name = BusName::try_from(BUS_NAME).ok()?;
    dbus.get_connection_unix_process_id(name).await.ok()
}

fn stop_xdg_desktop_portal() {
    tracing::info!("portal: stopping xdg-desktop-portal");
    let out = std::process::Command::new("systemctl")
        .args(["--user", "stop", "xdg-desktop-portal.service"])
        .output();
    match out {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!(
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "portal: cannot stop xdg-desktop-portal"
        ),
        Err(err) => tracing::warn!(?err, "portal: cannot run systemctl"),
    }
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

fn close_requests_from(shared: &Shared, tx: &Sender<PortalEvent>, sender: &str) {
    if let Ok(pending) = shared.pending.lock() {
        for p in pending.iter().filter(|p| p.sender.as_str() == sender) {
            if !p.closed.swap(true, Ordering::Relaxed) {
                tracing::info!(id = p.id, "portal: app left; closing its dialog");
                let _ = tx.send(PortalEvent::Close(p.id));
            }
        }
    }
}

/// Tell the app how request `p` ended (unless it was closed) and drop its Request object.
async fn finish(conn: &Connection, p: Pending, result: PickResult) {
    let _ = conn
        .object_server()
        .remove::<Request, _>(p.handle.as_ref())
        .await;
    note_outcome(match &result {
        PickResult::Chosen(paths) => format!("chose {} item(s)", paths.len()),
        PickResult::Cancelled => "cancelled".into(),
        PickResult::Failed if p.closed.load(Ordering::Relaxed) => "closed".into(),
        PickResult::Failed => "failed".into(),
    });
    if p.closed.load(Ordering::Relaxed) {
        return;
    }
    let (code, results) = match result {
        PickResult::Chosen(paths) => {
            let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
            let mut results = Options::new();
            if let Ok(v) = Variant::from(uris).try_into_owned() {
                results.insert("uris".into(), v);
            }
            (RESPONSE_OK, results)
        }
        PickResult::Cancelled => (RESPONSE_CANCELLED, Options::new()),
        PickResult::Failed => (RESPONSE_OTHER, Options::new()),
    };
    let sent = match SignalEmitter::new(conn, p.handle.as_ref()) {
        Ok(emitter) => {
            let emitter = emitter.set_destination(BusName::Unique(p.sender.as_ref().clone()));
            Request::response(&emitter, code, results).await
        }
        Err(err) => Err(err),
    };
    if let Err(err) = sent {
        tracing::warn!(?err, "portal: cannot send the response");
    }
}

/// `/org/freedesktop/portal/desktop/request/1_42/<token>` for sender `:1.42`. A missing
/// or invalid token is replaced by one made from `id`.
fn request_path(sender: &str, token: Option<&str>, id: u64) -> String {
    let sender = sender.trim_start_matches(':').replace('.', "_");
    let token = token
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .map(str::to_string)
        .unwrap_or_else(|| format!("simplewm{id}"));
    format!("{REQUEST_PATH}/{sender}/{token}")
}

struct FileChooser {
    tx: Sender<PortalEvent>,
    next_id: AtomicU64,
    shared: Arc<Shared>,
}

type Options = HashMap<String, OwnedVariant>;

#[interface(name = "org.freedesktop.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
        _parent_window: String,
        title: String,
        options: Options,
    ) -> fdo::Result<OwnedObjectPath> {
        let multiple = opt_bool(&options, "multiple");
        let directory = opt_bool(&options, "directory");
        let folder = opt_path(&options, "current_folder");
        let ask = Ask {
            kind: "Open",
            save: false,
            multiple,
            directory,
            title,
            folder,
            name: String::new(),
        };
        self.begin(&header, conn, server, ask, &options).await
    }

    async fn save_file(
        &self,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        #[zbus(object_server)] server: &ObjectServer,
        _parent_window: String,
        title: String,
        options: Options,
    ) -> fdo::Result<OwnedObjectPath> {
        let mut folder = opt_path(&options, "current_folder");
        let mut name = opt_str(&options, "current_name").unwrap_or_default();
        // An existing file being saved again: start next to it, with its name.
        if let Some(file) = opt_path(&options, "current_file") {
            if let Some(n) = file.file_name() {
                name = n.to_string_lossy().into_owned();
            }
            folder = file.parent().map(Path::to_path_buf).or(folder);
        }
        let ask = Ask {
            kind: "Save",
            save: true,
            multiple: false,
            directory: false,
            title,
            folder,
            name,
        };
        self.begin(&header, conn, server, ask, &options).await
    }

    /// Lower case, as the portal spec has it (zbus would capitalize it otherwise).
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        VERSION
    }
}

/// What a FileChooser call asks for.
struct Ask {
    kind: &'static str,
    save: bool,
    multiple: bool,
    directory: bool,
    title: String,
    folder: Option<PathBuf>,
    name: String,
}

impl FileChooser {
    /// Export the request object, hand the request to the compositor and return its
    /// handle; `serve` sends the answer when it comes.
    async fn begin(
        &self,
        header: &Header<'_>,
        conn: &Connection,
        server: &ObjectServer,
        ask: Ask,
        options: &Options,
    ) -> fdo::Result<OwnedObjectPath> {
        let sender: OwnedUniqueName = header
            .sender()
            .ok_or_else(|| fdo::Error::Failed("no sender".into()))?
            .to_owned()
            .into();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let path = request_path(
            sender.as_str(),
            opt_str(options, "handle_token").as_deref(),
            id,
        );
        let handle = OwnedObjectPath::try_from(path)
            .map_err(|e| fdo::Error::InvalidArgs(format!("bad request handle: {e}")))?;

        let app = app_name(conn, &sender).await;
        tracing::info!(
            app,
            title = ask.title,
            kind = ask.kind,
            "portal: file dialog"
        );

        let closed = Arc::new(AtomicBool::new(false));
        let request = Request {
            tx: self.tx.clone(),
            id,
            sender: sender.clone(),
            closed: closed.clone(),
        };
        if server.at(handle.as_ref(), request).await?.is_some() {
            return Err(fdo::Error::Failed("request handle already in use".into()));
        }
        note_request(ask.kind, &app, &ask.title);

        let (filters, current_filter) = parse_filters(options);
        let (reply, answer) = oneshot::channel();
        let pick = PickRequest {
            id,
            save: ask.save,
            multiple: ask.multiple,
            directory: ask.directory,
            title: ask.title,
            accept_label: opt_str(options, "accept_label").filter(|s| !s.is_empty()),
            folder: ask.folder,
            name: ask.name,
            filters,
            current_filter,
            reply: PickReply::Portal(reply),
        };
        if self.tx.send(PortalEvent::Pick(pick)).is_err() {
            let _ = server.remove::<Request, _>(handle.as_ref()).await;
            note_outcome("failed".into());
            return Err(fdo::Error::Failed("simplewm is shutting down".into()));
        }
        if let Ok(mut pending) = self.shared.pending.lock() {
            pending.push(Pending {
                id,
                sender,
                handle: handle.clone(),
                closed,
                answer,
            });
        }
        if let Some(waker) = self.shared.waker.lock().ok().and_then(|mut w| w.take()) {
            waker.wake();
        }
        Ok(handle)
    }
}

/// The calling program's name, for the status (its bus name if that's unknown).
async fn app_name(conn: &Connection, sender: &OwnedUniqueName) -> String {
    let pid = match DBusProxy::new(conn).await {
        Ok(dbus) => dbus
            .get_connection_unix_process_id(BusName::Unique(sender.as_ref().clone()))
            .await
            .ok(),
        Err(_) => None,
    };
    let name = pid.map(process_name).unwrap_or_default();
    let name = name.rsplit('/').next().unwrap_or_default();
    if name.is_empty() {
        sender.to_string()
    } else {
        name.to_string()
    }
}

/// Record a request in the status (finished by `note_outcome`).
fn note_request(kind: &str, app: &str, title: &str) {
    let time = chrono::Local::now().format("%H:%M:%S");
    update_status(|s| {
        s.requests += 1;
        s.last_request = Some(format!("{time} {kind} \"{title}\" from {app}: waiting"));
    });
}

fn note_outcome(outcome: String) {
    update_status(|s| {
        if let Some(last) = &mut s.last_request
            && let Some(stem) = last.strip_suffix("waiting")
        {
            *last = format!("{stem}{outcome}");
        }
    });
}

/// One file dialog, at its handle. The app may close it; `serve` removes it when done.
struct Request {
    tx: Sender<PortalEvent>,
    id: u64,
    sender: OwnedUniqueName,
    closed: Arc<AtomicBool>,
}

#[interface(name = "org.freedesktop.portal.Request")]
impl Request {
    async fn close(&self, #[zbus(header)] header: Header<'_>) -> fdo::Result<()> {
        if header.sender().map(|s| s.as_str()) != Some(self.sender.as_str()) {
            return Err(fdo::Error::AccessDenied("not your request".into()));
        }
        if !self.closed.swap(true, Ordering::Relaxed) {
            let _ = self.tx.send(PortalEvent::Close(self.id));
        }
        Ok(())
    }

    #[zbus(signal)]
    async fn response(
        emitter: &SignalEmitter<'_>,
        response: u32,
        results: Options,
    ) -> zbus::Result<()>;
}

// --- option parsing ----------------------------------------------------------

fn opt_bool(options: &Options, key: &str) -> bool {
    options
        .get(key)
        .and_then(|v| v.downcast_ref::<bool>().ok())
        .unwrap_or(false)
}

fn opt_str(options: &Options, key: &str) -> Option<String> {
    options
        .get(key)
        .and_then(|v| v.downcast_ref::<String>().ok())
}

/// A path option, sent as a NUL-terminated byte array.
fn opt_path(options: &Options, key: &str) -> Option<PathBuf> {
    let v = options.get(key)?.try_clone().ok()?;
    let mut bytes = Vec::<u8>::try_from(v).ok()?;
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    (!bytes.is_empty()).then(|| PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// `(name, [(kind, pattern)])`: kind 0 is a glob, 1 a mime type.
type RawFilter = (String, Vec<(u32, String)>);

fn parse_filters(options: &Options) -> (Vec<Filter>, Option<usize>) {
    let raw = |key: &str| options.get(key).and_then(|v| v.try_clone().ok());
    let mut filters: Vec<Filter> = raw("filters")
        .and_then(|v| Vec::<RawFilter>::try_from(v).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(to_filter)
        .collect();
    let current = raw("current_filter")
        .and_then(|v| RawFilter::try_from(v).ok())
        .and_then(to_filter)
        .map(
            |cur| match filters.iter().position(|f| f.name == cur.name) {
                Some(i) => i,
                None => {
                    filters.push(cur);
                    filters.len() - 1
                }
            },
        );
    (filters, current)
}

fn to_filter((name, patterns): RawFilter) -> Option<Filter> {
    let mut globs = Vec::new();
    for (kind, pattern) in patterns {
        match kind {
            0 => globs.push(pattern.to_lowercase()),
            1 => globs.extend(mime_globs(&pattern)),
            _ => {}
        }
    }
    (!globs.is_empty()).then_some(Filter { name, globs })
}

/// Globs for a mime type (`image/png`, or `image/*`) from the shared-mime-info database.
fn mime_globs(mime: &str) -> Vec<String> {
    static GLOBS: OnceLock<Vec<(String, String)>> = OnceLock::new();
    let table = GLOBS.get_or_init(|| {
        // Lines are `weight:mime/type:glob[:flags]`.
        std::fs::read_to_string("/usr/share/mime/globs2")
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut parts = l.split(':');
                let (_, mime, glob) = (parts.next()?, parts.next()?, parts.next()?);
                Some((mime.to_string(), glob.to_lowercase()))
            })
            .collect()
    });
    let prefix = mime.strip_suffix('*');
    let mut globs: Vec<String> = table
        .iter()
        .filter(|(m, _)| match prefix {
            Some(p) => m.starts_with(p),
            None => m == mime,
        })
        .map(|(_, g)| g.clone())
        .collect();
    globs.dedup();
    if globs.is_empty() {
        tracing::debug!(mime, "portal: no globs for mime type");
    }
    globs
}

/// Shell-style glob match supporting `*`, `?` and `[...]` classes.
fn glob_match(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| glob_match(rest, &name[i..])),
        Some((b'?', rest)) => !name.is_empty() && glob_match(rest, &name[1..]),
        Some((b'[', rest)) => {
            let Some(end) = rest.iter().skip(1).position(|&c| c == b']').map(|e| e + 1) else {
                return name.first() == Some(&b'[') && glob_match(rest, &name[1..]);
            };
            let Some((&c, tail)) = name.split_first() else {
                return false;
            };
            let (negate, class) = match rest[..end].split_first() {
                Some((b'!' | b'^', class)) => (true, class),
                _ => (false, &rest[..end]),
            };
            let mut hit = false;
            let mut i = 0;
            while i < class.len() {
                if i + 2 < class.len() && class[i + 1] == b'-' {
                    hit |= (class[i]..=class[i + 2]).contains(&c);
                    i += 3;
                } else {
                    hit |= class[i] == c;
                    i += 1;
                }
            }
            hit != negate && glob_match(&rest[end + 1..], tail)
        }
        Some((&p, rest)) => name.first() == Some(&p) && glob_match(rest, &name[1..]),
    }
}

/// `file://` URI for an absolute path, percent-encoding everything but unreserved
/// characters and `/`.
fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match(b"*.png", b"a.png"));
        assert!(!glob_match(b"*.png", b"a.pngx"));
        assert!(glob_match(b"*.[jp][pn]g", b"x.jpg"));
        assert!(glob_match(b"*.[!a]", b"x.b"));
        assert!(!glob_match(b"*.[!a]", b"x.a"));
        assert!(glob_match(b"file?.txt", b"file1.txt"));
        assert!(glob_match(b"*", b""));
    }

    #[test]
    fn uris() {
        assert_eq!(
            file_uri(Path::new("/tmp/a b/ü.txt")),
            "file:///tmp/a%20b/%C3%BC.txt"
        );
    }

    #[test]
    fn request_paths() {
        assert_eq!(
            request_path(":1.42", Some("gtk12"), 7),
            "/org/freedesktop/portal/desktop/request/1_42/gtk12"
        );
        // No token, or one that isn't a valid path element: we make one up.
        assert_eq!(
            request_path(":1.42", None, 7),
            "/org/freedesktop/portal/desktop/request/1_42/simplewm7"
        );
        assert_eq!(
            request_path(":1.42", Some("a/b"), 8),
            "/org/freedesktop/portal/desktop/request/1_42/simplewm8"
        );
        assert_eq!(
            request_path(":1.42", Some(""), 9),
            "/org/freedesktop/portal/desktop/request/1_42/simplewm9"
        );
    }
}

/// The portal over a real (private) session bus, called the way apps call it. Run inside
/// a scratch bus: `dbus-run-session -- cargo test portal_bus_test -- --ignored`.
#[cfg(test)]
mod portal_bus_test {
    use std::time::{Duration, Instant};

    use smithay::reexports::calloop::channel::{Channel, channel};
    use zbus::{Connection, OwnedVariant, Proxy, Variant};

    use super::*;

    const CHOOSER: &str = "org.freedesktop.portal.FileChooser";
    const REQUEST: &str = "org.freedesktop.portal.Request";

    /// The Response signal's body: response code and results.
    type Response = (u32, HashMap<String, OwnedVariant>);

    async fn next<S: Stream + Unpin>(s: &mut S) -> Option<S::Item> {
        std::future::poll_fn(|cx| Pin::new(&mut *s).poll_next(cx)).await
    }

    /// Where an app connected as `conn` expects the answer to a call with `token`.
    fn expected_handle(conn: &Connection, token: &str) -> String {
        request_path(conn.unique_name().unwrap().as_str(), Some(token), 0)
    }

    async fn ask(
        conn: &Connection,
        method: &str,
        options: HashMap<&'static str, Variant<'static>>,
    ) -> OwnedObjectPath {
        conn.call_method(
            Some(BUS_NAME),
            OBJECT_PATH,
            Some(CHOOSER),
            method,
            &("", "Pick something", options),
        )
        .await
        .unwrap()
        .body()
        .deserialize()
        .unwrap()
    }

    /// Do what an app does: subscribe to the handle's Response, call `method`, and wait
    /// for the answer, on its own thread.
    fn call(
        method: &'static str,
        token: &'static str,
        mut options: HashMap<&'static str, Variant<'static>>,
    ) -> std::thread::JoinHandle<Response> {
        std::thread::spawn(move || {
            zbus::block_on(async move {
                let conn = Connection::session().await.unwrap();
                let path = expected_handle(&conn, token);
                let proxy = Proxy::new(&conn, BUS_NAME, path.clone(), REQUEST)
                    .await
                    .unwrap();
                let mut responses = proxy.receive_signal("Response").await.unwrap();
                options.insert("handle_token", Variant::from(token));
                let handle = ask(&conn, method, options).await;
                assert_eq!(handle.as_str(), path);
                let msg = next(&mut responses).await.unwrap();
                msg.body().deserialize::<Response>().unwrap()
            })
        })
    }

    fn next_event(rx: &Channel<PortalEvent>) -> PortalEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(event) = rx.try_recv() {
                return event;
            }
            assert!(Instant::now() < deadline, "no event from the portal");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn next_pick(rx: &Channel<PortalEvent>) -> PickRequest {
        match next_event(rx) {
            PortalEvent::Pick(req) => req,
            PortalEvent::Close(id) => panic!("expected a pick, got close of {id}"),
        }
    }

    fn expect_close(rx: &Channel<PortalEvent>, id: u64) {
        match next_event(rx) {
            PortalEvent::Close(closed) => assert_eq!(closed, id),
            PortalEvent::Pick(_) => panic!("expected close"),
        }
    }

    fn answer(req: PickRequest, result: PickResult) {
        match req.reply {
            PickReply::Portal(tx) => tx.send(result).ok().unwrap(),
            _ => panic!("not a portal request"),
        }
    }

    #[test]
    #[ignore]
    fn serves_file_dialogs() {
        // Someone else has the name first: simplewm waits, and takes over when it's let go.
        let holder = zbus::block_on(async {
            let conn = Connection::session().await.unwrap();
            conn.request_name(BUS_NAME).await.unwrap();
            conn
        });
        let (tx, rx) = channel();
        start(tx);
        let wait_for = |want: fn(&State) -> bool, what: &str| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let state = status().state;
                if want(&state) {
                    return state;
                }
                if let State::Failed(why) = state {
                    panic!("{why}");
                }
                assert!(Instant::now() < deadline, "portal never got to {what}");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let waiting = wait_for(|s| matches!(s, State::Waiting { .. }), "waiting");
        let State::Waiting { owner } = waiting else {
            unreachable!()
        };
        assert_eq!(owner, Some(std::process::id()));
        zbus::block_on(holder.release_name(BUS_NAME)).unwrap();
        wait_for(|s| matches!(s, State::Serving), "serving");

        // Apps check the version first.
        zbus::block_on(async {
            let conn = Connection::session().await.unwrap();
            let proxy = Proxy::new(&conn, BUS_NAME, OBJECT_PATH, CHOOSER)
                .await
                .unwrap();
            assert_eq!(proxy.get_property::<u32>("version").await.unwrap(), VERSION);
        });

        // Open, several files, with a filter: the chosen paths come back as file URIs.
        let filters = vec![("Images", vec![(0u32, "*.PNG")])];
        let app = call(
            "OpenFile",
            "t1",
            HashMap::from([
                ("multiple", Variant::from(true)),
                ("filters", Variant::from(filters)),
            ]),
        );
        let req = next_pick(&rx);
        assert!(!req.save && req.multiple);
        assert_eq!(req.title, "Pick something");
        assert_eq!(req.filters.len(), 1);
        assert!(req.filters[0].matches("a.png"));
        answer(
            req,
            PickResult::Chosen(vec!["/tmp/a b.png".into(), "/tmp/c.png".into()]),
        );
        let (code, results) = app.join().unwrap();
        assert_eq!(code, RESPONSE_OK);
        let uris = Vec::<String>::try_from(results["uris"].try_clone().unwrap()).unwrap();
        assert_eq!(uris, ["file:///tmp/a%20b.png", "file:///tmp/c.png"]);

        // Save next to an existing file, then cancel.
        let current_file = b"/home/u/notes.txt\0".to_vec();
        let app = call(
            "SaveFile",
            "t2",
            HashMap::from([("current_file", Variant::from(current_file))]),
        );
        let req = next_pick(&rx);
        assert!(req.save);
        assert_eq!(req.name, "notes.txt");
        assert_eq!(req.folder, Some("/home/u".into()));
        answer(req, PickResult::Cancelled);
        assert_eq!(app.join().unwrap().0, RESPONSE_CANCELLED);

        // Only the app that asked may close its request; when it does, the dialog closes.
        let (handle_tx, handle_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let app = std::thread::spawn(move || {
            zbus::block_on(async move {
                let conn = Connection::session().await.unwrap();
                let handle = ask(&conn, "OpenFile", HashMap::new()).await;
                handle_tx.send(handle.clone()).unwrap();
                go_rx.recv().unwrap();
                conn.call_method(Some(BUS_NAME), handle.as_str(), Some(REQUEST), "Close", &())
                    .await
                    .unwrap();
            })
        });
        let req = next_pick(&rx);
        let handle = handle_rx.recv().unwrap();
        zbus::block_on(async {
            let stranger = Connection::session().await.unwrap();
            let denied = stranger
                .call_method(Some(BUS_NAME), handle.as_str(), Some(REQUEST), "Close", &())
                .await;
            assert!(denied.is_err(), "a stranger closed the request");
        });
        go_tx.send(()).unwrap();
        app.join().unwrap();
        expect_close(&rx, req.id);
        answer(req, PickResult::Failed);

        // An app that quits with a dialog open gets the dialog closed.
        let app = std::thread::spawn(|| {
            zbus::block_on(async {
                let conn = Connection::session().await.unwrap();
                ask(&conn, "OpenFile", HashMap::new()).await;
            })
        });
        let req = next_pick(&rx);
        app.join().unwrap();
        expect_close(&rx, req.id);
        answer(req, PickResult::Failed);
    }
}
