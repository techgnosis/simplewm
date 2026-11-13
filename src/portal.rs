//! simplewm as an `xdg-desktop-portal` backend for file chooser dialogs.
//!
//! Apps ask the portal frontend (`xdg-desktop-portal`, started on demand by D-Bus) for a
//! file dialog. With `simplewm.portal` installed and `simplewm-portals.conf` selecting it,
//! the frontend passes the request to us on `org.freedesktop.impl.portal.FileChooser`.
//! We show the File Picker applet and reply once the user picks or cancels.
//!
//! D-Bus runs on a "portal" thread that sits in `zbus::block_on` for good, which is what
//! drives the connection. Requests reach the compositor through a calloop channel; each
//! carries a oneshot sender for the answer, which the D-Bus method awaits before replying.

use std::{
    collections::HashMap,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use futures_channel::oneshot;
use smithay::reexports::calloop::channel::Sender;
use zbus::{
    ObjectServer, OwnedObjectPath, OwnedVariant, Variant,
    fdo::{RequestNameFlags, RequestNameReply},
    interface,
};

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.simplewm";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// Portal response codes.
const RESPONSE_OK: u32 = 0;
const RESPONSE_CANCELLED: u32 = 1;
const RESPONSE_OTHER: u32 = 2;

/// Sent from the D-Bus thread to the compositor.
pub enum PortalEvent {
    Pick(PickRequest),
    /// The frontend closed request `.0` (e.g. the app went away).
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

/// How the backend is doing, shown in the Desktop Portal applet.
#[derive(Clone, Default)]
pub struct BackendStatus {
    /// We own `BUS_NAME` and serve requests.
    pub serving: bool,
    /// Why we are not serving, if we gave up.
    pub problem: Option<String>,
    /// File dialog requests received since startup.
    pub requests: u64,
    /// The most recent request and how it ended.
    pub last_request: Option<String>,
}

static STATUS: Mutex<BackendStatus> = Mutex::new(BackendStatus {
    serving: false,
    problem: None,
    requests: 0,
    last_request: None,
});

pub fn status() -> BackendStatus {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn update_status(f: impl FnOnce(&mut BackendStatus)) {
    if let Ok(mut s) = STATUS.lock() {
        f(&mut s);
    }
}

fn set_problem(problem: String) {
    update_status(|s| {
        s.serving = false;
        s.problem = Some(problem);
    });
}

/// Start serving the portal backend on the session bus (in the background).
pub fn start(tx: Sender<PortalEvent>) {
    let spawned = std::thread::Builder::new()
        .name("portal".into())
        .spawn(move || {
            let chooser = FileChooser {
                tx,
                next_id: AtomicU64::new(1),
            };
            // The connection only runs while this thread is inside block_on, so serve()
            // never returns once the backend is up.
            zbus::block_on(serve(chooser));
        });
    if let Err(err) = spawned {
        tracing::warn!(?err, "portal: cannot start thread");
        set_problem(format!("cannot start the D-Bus thread: {err}"));
    }
}

/// Connect, take the bus name and serve requests forever (or return on failure).
async fn serve(chooser: FileChooser) {
    let conn = zbus::connection::Builder::session()
        .serve_at(OBJECT_PATH, chooser)
        .build()
        .await;
    let conn = match conn {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(?err, "portal: cannot connect to the session bus");
            set_problem(format!("cannot connect to the session bus: {err}"));
            return;
        }
    };
    // Don't take the name from another running simplewm.
    match conn
        .request_name_with_flags(BUS_NAME, RequestNameFlags::DO_NOT_QUEUE)
        .await
    {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {
            tracing::info!(name = BUS_NAME, "portal: file chooser backend ready");
            update_status(|s| s.serving = true);
        }
        Ok(reply) => {
            tracing::warn!(
                ?reply,
                name = BUS_NAME,
                "portal: name already owned elsewhere"
            );
            set_problem("the bus name is owned by another process (another simplewm?)".into());
            return;
        }
        Err(err) => {
            tracing::warn!(?err, name = BUS_NAME, "portal: cannot own bus name");
            set_problem(format!("cannot own the bus name: {err}"));
            return;
        }
    }
    std::future::pending::<()>().await;
}

struct FileChooser {
    tx: Sender<PortalEvent>,
    next_id: AtomicU64,
}

type Options = HashMap<String, OwnedVariant>;

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        title: String,
        options: Options,
    ) -> (u32, Options) {
        tracing::info!(app_id, title, "portal: open file");
        note_request("Open", &app_id, &title);
        let multiple = opt_bool(&options, "multiple");
        let directory = opt_bool(&options, "directory");
        let folder = opt_path(&options, "current_folder");
        self.pick(
            server,
            handle,
            false,
            multiple,
            directory,
            title,
            folder,
            String::new(),
            &options,
        )
        .await
    }

    async fn save_file(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
        handle: OwnedObjectPath,
        app_id: String,
        _parent_window: String,
        title: String,
        options: Options,
    ) -> (u32, Options) {
        tracing::info!(app_id, title, "portal: save file");
        note_request("Save", &app_id, &title);
        let mut folder = opt_path(&options, "current_folder");
        let mut name = opt_str(&options, "current_name").unwrap_or_default();
        // An existing file being saved again: start next to it, with its name.
        if let Some(file) = opt_path(&options, "current_file") {
            if let Some(n) = file.file_name() {
                name = n.to_string_lossy().into_owned();
            }
            folder = file.parent().map(Path::to_path_buf).or(folder);
        }
        self.pick(
            server, handle, true, false, false, title, folder, name, &options,
        )
        .await
    }
}

impl FileChooser {
    #[allow(clippy::too_many_arguments)]
    async fn pick(
        &self,
        server: &ObjectServer,
        handle: OwnedObjectPath,
        save: bool,
        multiple: bool,
        directory: bool,
        title: String,
        folder: Option<PathBuf>,
        name: String,
        options: &Options,
    ) -> (u32, Options) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (filters, current_filter) = parse_filters(options);
        let (reply, answer) = oneshot::channel();
        let request = PickRequest {
            id,
            save,
            multiple,
            directory,
            title,
            accept_label: opt_str(options, "accept_label").filter(|s| !s.is_empty()),
            folder,
            name,
            filters,
            current_filter,
            reply: PickReply::Portal(reply),
        };

        // The frontend calls Close on this object to cancel the dialog.
        let close = Request {
            tx: self.tx.clone(),
            id,
        };
        if let Err(err) = server.at(&handle, close).await {
            tracing::warn!(?err, "portal: cannot export request object");
        }
        let result = if self.tx.send(PortalEvent::Pick(request)).is_ok() {
            answer.await.unwrap_or(PickResult::Failed)
        } else {
            PickResult::Failed
        };
        let _ = server.remove::<Request, _>(&handle).await;

        note_outcome(match &result {
            PickResult::Chosen(paths) => format!("chose {} item(s)", paths.len()),
            PickResult::Cancelled => "cancelled".into(),
            PickResult::Failed => "failed".into(),
        });
        match result {
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
        }
    }
}

/// Record a request in the status (finished by `note_outcome`).
fn note_request(kind: &str, app_id: &str, title: &str) {
    let time = chrono::Local::now().format("%H:%M:%S");
    let app = if app_id.is_empty() {
        "an unsandboxed app"
    } else {
        app_id
    };
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

struct Request {
    tx: Sender<PortalEvent>,
    id: u64,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    async fn close(&self) {
        let _ = self.tx.send(PortalEvent::Close(self.id));
    }
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
}

/// The backend over a real (private) session bus: requests reach the compositor side and
/// answers reach the app. Run inside a scratch bus:
/// `dbus-run-session -- cargo test portal_bus_test -- --ignored`.
#[cfg(test)]
mod portal_bus_test {
    use std::time::{Duration, Instant};

    use smithay::reexports::calloop::channel::{Channel, channel};
    use zbus::{Connection, OwnedVariant, Variant};

    use super::*;

    const CHOOSER: &str = "org.freedesktop.impl.portal.FileChooser";

    /// What an app's request comes back with: response code and results.
    type Response = (u32, HashMap<String, OwnedVariant>);

    /// Call `method` as an app would, on its own thread, and hand back the answer.
    fn call(
        method: &'static str,
        handle: &'static str,
        options: HashMap<&'static str, Variant<'static>>,
    ) -> std::thread::JoinHandle<Response> {
        std::thread::spawn(move || {
            zbus::block_on(async move {
                let conn = Connection::session().await.unwrap();
                let handle = zbus::ObjectPath::try_from(handle).unwrap();
                let reply = conn
                    .call_method(
                        Some(BUS_NAME),
                        OBJECT_PATH,
                        Some(CHOOSER),
                        method,
                        &(handle, "org.example.App", "", "Pick something", options),
                    )
                    .await
                    .unwrap();
                reply.body().deserialize::<Response>().unwrap()
            })
        })
    }

    fn next_pick(rx: &Channel<PortalEvent>) -> PickRequest {
        match rx.recv().unwrap() {
            PortalEvent::Pick(req) => req,
            PortalEvent::Close(id) => panic!("expected a pick, got close of {id}"),
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
        let (tx, rx) = channel();
        start(tx);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !status().serving {
            assert!(status().problem.is_none(), "{:?}", status().problem);
            assert!(Instant::now() < deadline, "backend did not start");
            std::thread::sleep(Duration::from_millis(20));
        }

        // Open, several files, with a filter: the chosen paths come back as file URIs.
        let filters = vec![("Images", vec![(0u32, "*.PNG")])];
        let app = call(
            "OpenFile",
            "/org/freedesktop/portal/desktop/request/1_1/t1",
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
            "/org/freedesktop/portal/desktop/request/1_1/t2",
            HashMap::from([("current_file", Variant::from(current_file))]),
        );
        let req = next_pick(&rx);
        assert!(req.save);
        assert_eq!(req.name, "notes.txt");
        assert_eq!(req.folder, Some("/home/u".into()));
        answer(req, PickResult::Cancelled);
        assert_eq!(app.join().unwrap().0, RESPONSE_CANCELLED);

        // The frontend closing a request reaches the compositor as Close.
        let handle = "/org/freedesktop/portal/desktop/request/1_1/t3";
        let app = call("OpenFile", handle, HashMap::new());
        let req = next_pick(&rx);
        zbus::block_on(async {
            let conn = Connection::session().await.unwrap();
            conn.call_method(
                Some(BUS_NAME),
                handle,
                Some("org.freedesktop.impl.portal.Request"),
                "Close",
                &(),
            )
            .await
            .unwrap();
        });
        match rx.recv().unwrap() {
            PortalEvent::Close(id) => assert_eq!(id, req.id),
            PortalEvent::Pick(_) => panic!("expected close"),
        }
        answer(req, PickResult::Cancelled);
        assert_eq!(app.join().unwrap().0, RESPONSE_CANCELLED);
    }
}
