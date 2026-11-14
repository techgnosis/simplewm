//! Settings storage: a small binary file at ~/.config/simplewm/simplewm.dat.
//!
//! It holds the Launcher menu's executables (in order) and key/value settings
//! (the screenshot folder, the Network applet's address, gateway and DNS
//! server). It's binary so that nobody is tempted to edit it by hand; the
//! Control Panel is the only way to change settings.
//!
//! Format (integers are little-endian u32, strings are a length then UTF-8):
//!
//! ```text
//! MAGIC  VERSION  entry-count  entry…  setting-count  (key value)…
//! ```
//!
//! To change the format, bump `VERSION` and keep `decode` reading the old one.
//! Every save writes a temporary file and renames it into place, so a crash
//! leaves either the old settings or the new ones. A file that can't be read
//! is moved aside to `simplewm.dat.bad` and simplewm starts with defaults.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use super::DEFAULT_LAUNCHER_ENTRIES;

const MAGIC: &[u8; 8] = b"simplewm";
const VERSION: u32 = 1;

pub fn store_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/simplewm/simplewm.dat"))
}

/// Everything simplewm remembers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Data {
    pub entries: Vec<String>,
    pub settings: BTreeMap<String, String>,
}

impl Default for Data {
    /// First run: the default launcher entries. After the first save they can be
    /// removed and stay removed.
    fn default() -> Self {
        Data {
            entries: DEFAULT_LAUNCHER_ENTRIES.map(String::from).to_vec(),
            settings: BTreeMap::new(),
        }
    }
}

/// The settings and where they're saved (`None`: not remembered).
pub(super) struct Store {
    path: Option<PathBuf>,
    data: Data,
}

impl Store {
    /// Load the settings file, or start with defaults when there is none.
    pub fn open() -> Self {
        let path = store_path();
        let data = path.as_deref().map(load).unwrap_or_default();
        Store { path, data }
    }

    pub fn entries(&self) -> &[String] {
        &self.data.entries
    }

    pub fn setting(&self, key: &str) -> Option<String> {
        self.data.settings.get(key).cloned()
    }

    /// Replace the launcher entries with `paths`, in order, and save.
    pub fn save_entries(&mut self, paths: &[String]) {
        self.data.entries = paths.to_vec();
        self.save();
    }

    /// Set `(key, value)` pairs and save.
    pub fn save_settings(&mut self, pairs: &[(&str, &str)]) {
        for (key, value) in pairs {
            self.data
                .settings
                .insert(key.to_string(), value.to_string());
        }
        self.save();
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(err) = write_atomic(path, &encode(&self.data)) {
            tracing::error!(?err, ?path, "failed to save settings");
        }
    }
}

/// Read the settings at `path`: defaults if it doesn't exist, and defaults
/// (with the file moved aside) if it can't be read.
fn load(path: &Path) -> Data {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Data::default(),
        Err(err) => {
            tracing::error!(?err, ?path, "failed to read settings");
            return Data::default();
        }
    };
    match decode(&bytes) {
        Ok(data) => {
            tracing::info!(?path, "loaded settings");
            data
        }
        Err(why) => {
            let bad = path.with_extension("dat.bad");
            tracing::error!(
                why,
                ?path,
                ?bad,
                "settings file is damaged; starting with defaults"
            );
            let _ = fs::rename(path, &bad);
            Data::default()
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("dat.tmp");
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        // Make the rename itself durable.
        fs::File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn encode(data: &Data) -> Vec<u8> {
    fn put_u32(out: &mut Vec<u8>, n: usize) {
        out.extend_from_slice(&(n as u32).to_le_bytes());
    }
    fn put_str(out: &mut Vec<u8>, s: &str) {
        put_u32(out, s.len());
        out.extend_from_slice(s.as_bytes());
    }
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&VERSION.to_le_bytes());
    put_u32(&mut out, data.entries.len());
    for e in &data.entries {
        put_str(&mut out, e);
    }
    put_u32(&mut out, data.settings.len());
    for (k, v) in &data.settings {
        put_str(&mut out, k);
        put_str(&mut out, v);
    }
    out
}

fn decode(bytes: &[u8]) -> Result<Data, &'static str> {
    struct Reader<'a>(&'a [u8]);
    impl<'a> Reader<'a> {
        fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
            if n > self.0.len() {
                return Err("file ends early");
            }
            let (head, rest) = self.0.split_at(n);
            self.0 = rest;
            Ok(head)
        }
        fn u32(&mut self) -> Result<usize, &'static str> {
            let b = self.take(4)?;
            Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        }
        fn string(&mut self) -> Result<String, &'static str> {
            let n = self.u32()?;
            let b = self.take(n)?;
            String::from_utf8(b.to_vec()).map_err(|_| "text is not UTF-8")
        }
    }

    let mut r = Reader(bytes);
    if r.take(MAGIC.len()).ok() != Some(MAGIC.as_slice()) {
        return Err("not a simplewm settings file");
    }
    if r.u32()? != VERSION as usize {
        return Err("unknown version");
    }
    // Counts aren't trusted for allocation: each item reads real bytes.
    let mut entries = Vec::new();
    for _ in 0..r.u32()? {
        entries.push(r.string()?);
    }
    let mut settings = BTreeMap::new();
    for _ in 0..r.u32()? {
        let key = r.string()?;
        settings.insert(key, r.string()?);
    }
    if !r.0.is_empty() {
        return Err("unexpected bytes at the end");
    }
    Ok(Data { entries, settings })
}

impl super::Panels {
    /// Store `(key, value)` pairs in the settings.
    pub(super) fn save_settings(&mut self, pairs: &[(&str, &str)]) {
        self.store.save_settings(pairs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("simplewm-store-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("simplewm.dat")
    }

    fn sample() -> Data {
        Data {
            entries: ["/usr/bin/b", "/usr/bin/a", "/usr/bin/ç"]
                .map(String::from)
                .to_vec(),
            settings: [("net_address", "10.0.0.2/24"), ("net_gateway", "")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .into(),
        }
    }

    #[test]
    fn round_trips() {
        let data = sample();
        assert_eq!(decode(&encode(&data)), Ok(data));
        let empty = Data {
            entries: vec![],
            settings: BTreeMap::new(),
        };
        assert_eq!(decode(&encode(&empty)), Ok(empty));
    }

    #[test]
    fn is_not_plain_text() {
        assert!(encode(&sample()).contains(&0));
    }

    #[test]
    fn rejects_damaged_files() {
        let good = encode(&sample());
        for n in 0..good.len() {
            assert!(decode(&good[..n]).is_err(), "truncated at {n}");
        }
        let mut extra = good.clone();
        extra.push(0);
        assert!(decode(&extra).is_err());
        let mut version = good.clone();
        version[8] = 9;
        assert_eq!(decode(&version), Err("unknown version"));
        // A huge count with no data behind it fails instead of allocating.
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&VERSION.to_le_bytes());
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&huge).is_err());
        assert!(decode(b"[launcher]\npath=/usr/bin/foot\n").is_err());
    }

    #[test]
    fn saves_and_loads_a_file() {
        let path = scratch("save");
        assert_eq!(load(&path), Data::default());
        let mut store = Store {
            path: Some(path.clone()),
            data: load(&path),
        };
        store.save_entries(&[]);
        store.save_settings(&[("net_address", "10.0.0.2/24")]);
        store.save_settings(&[("net_address", "10.0.0.3/24"), ("screenshot_dir", "/tmp")]);

        let again = Store {
            path: Some(path.clone()),
            data: load(&path),
        };
        // A removed default entry stays removed.
        assert!(again.entries().is_empty());
        assert_eq!(again.setting("net_address").as_deref(), Some("10.0.0.3/24"));
        assert_eq!(again.setting("screenshot_dir").as_deref(), Some("/tmp"));
        assert_eq!(again.setting("net_dns"), None);
        assert!(!path.with_extension("dat.tmp").exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn damaged_file_is_moved_aside() {
        let path = scratch("bad");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"garbage").unwrap();
        assert_eq!(load(&path), Data::default());
        assert!(!path.exists());
        assert_eq!(
            fs::read(path.with_extension("dat.bad")).unwrap(),
            b"garbage"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
