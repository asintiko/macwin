//! Copied files: what is sent ([`build_manifest`]), streaming their data
//! ([`send_data`]), and receiving them into a staging directory
//! ([`receive_into`]) whose top-level items are then put on the clipboard,
//! so that pasting in Finder / Explorer copies them out.
//!
//! A copy is sent completely or not at all: an item that cannot be read,
//! or whose name cannot travel, refuses the whole copy instead of leaving a
//! hole in a pasted folder. Symlinks (and sockets, pipes) are the exception:
//! they are never followed or sent.
//!
//! Received names are mapped to what the local file system accepts
//! (Windows: no `<>:"\|?*`, no trailing dots or spaces, no reserved device
//! names) and made unique case-insensitively, so two entries never land on
//! the same file.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use macwin_core::clipboard::LocalFiles;
use macwin_core::msg::{FileEntry, FilesHeader};
use macwin_net::link::IncomingFiles;
use macwin_proto::{MAX_FILE_DEPTH, MAX_FILE_ENTRIES, MAX_FILES_HEADER, check_relative_path};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

/// Chunk size of file reads and writes.
const COPY_BUF: usize = 256 * 1024;
/// Upper bound of the encoded size of a header entry besides its path.
const ENTRY_OVERHEAD: usize = 40;
/// Other names tried when the file system refuses a mapped one.
const MAX_RENAMES: u32 = 100;

/// Why copied files cannot be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// More data than allowed; `bytes` is how much was counted so far.
    TooLarge { bytes: u64 },
    /// More items, nesting or name bytes than one transfer carries.
    TooMany,
    /// An item cannot be read, or its name cannot be sent.
    Unsendable(&'static str),
    /// Nothing sendable (only symlinks).
    Empty,
}

/// Never names a file.
impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::TooLarge { bytes } => {
                write!(f, "over the size limit ({bytes} bytes counted)")
            }
            ManifestError::TooMany => write!(
                f,
                "more than {MAX_FILE_ENTRIES} items, {MAX_FILE_DEPTH} levels or 16 MiB of names"
            ),
            ManifestError::Unsendable(why) => f.write_str(why),
            ManifestError::Empty => f.write_str("nothing to send"),
        }
    }
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

struct Walker {
    max_bytes: u64,
    total: u64,
    /// Upper bound of the encoded header so far.
    header_bytes: usize,
    entries: Vec<FileEntry>,
    sources: Vec<PathBuf>,
}

impl Walker {
    fn push(&mut self, entry: FileEntry, source: PathBuf) -> Result<(), ManifestError> {
        self.header_bytes += entry.path.len() + ENTRY_OVERHEAD;
        if self.entries.len() >= MAX_FILE_ENTRIES || self.header_bytes > MAX_FILES_HEADER {
            return Err(ManifestError::TooMany);
        }
        self.entries.push(entry);
        self.sources.push(source);
        Ok(())
    }

    fn add(&mut self, rel: String, path: PathBuf, depth: usize) -> Result<(), ManifestError> {
        if depth > MAX_FILE_DEPTH {
            return Err(ManifestError::TooMany);
        }
        if check_relative_path(&rel).is_err() {
            return Err(ManifestError::Unsendable(
                "a name cannot be sent (a backslash, or over 255 bytes)",
            ));
        }
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|_| ManifestError::Unsendable("an item cannot be read"))?;
        if meta.file_type().is_symlink() {
            debug!("skipping a symlink");
            return Ok(());
        }
        if meta.is_dir() {
            let entry = FileEntry {
                path: rel.clone(),
                size: 0,
                dir: true,
                executable: false,
                mtime_ms: mtime_ms(&meta),
            };
            self.push(entry, path.clone())?;
            let unreadable = |_| ManifestError::Unsendable("a folder cannot be read");
            let mut children = std::fs::read_dir(&path)
                .map_err(unreadable)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(unreadable)?;
            children.sort_by_key(|c| c.file_name());
            for child in children {
                let name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| ManifestError::Unsendable("a name is not valid Unicode"))?;
                self.add(format!("{rel}/{name}"), child.path(), depth + 1)?;
            }
        } else if meta.is_file() {
            self.total = self.total.saturating_add(meta.len());
            if self.total > self.max_bytes {
                return Err(ManifestError::TooLarge { bytes: self.total });
            }
            let entry = FileEntry {
                path: rel,
                size: meta.len(),
                dir: false,
                executable: is_executable(&meta),
                mtime_ms: mtime_ms(&meta),
            };
            self.push(entry, path)?;
        } else {
            debug!("skipping a special file");
        }
        Ok(())
    }
}

/// The entries of a transfer of the copied `roots` (files and folders,
/// recursively). Each root keeps its own name; two roots with the same name
/// get " (2)" and so on.
pub fn build_manifest(roots: &[PathBuf], max_bytes: u64) -> Result<LocalFiles, ManifestError> {
    let mut walker = Walker {
        max_bytes,
        total: 0,
        header_bytes: 0,
        entries: Vec::new(),
        sources: Vec::new(),
    };
    let mut top_names = HashSet::new();
    for root in roots {
        let name = root
            .file_name()
            .ok_or(ManifestError::Unsendable("a whole drive cannot be sent"))?
            .to_str()
            .ok_or(ManifestError::Unsendable("a name is not valid Unicode"))?;
        let mut unique = name.to_string();
        let mut n = 2;
        while !top_names.insert(unique.to_lowercase()) {
            unique = with_suffix(name, n);
            n += 1;
        }
        walker.add(unique, root.clone(), 1)?;
    }
    if walker.entries.is_empty() {
        return Err(ManifestError::Empty);
    }
    Ok(LocalFiles {
        entries: walker.entries,
        sources: walker.sources,
    })
}

/// Streams the data of `files` in header order. Fails if a file shrank
/// since the manifest was built (the receiver then discards the transfer).
pub async fn send_data(
    out: &mut (impl AsyncWrite + Unpin),
    entries: &[FileEntry],
    sources: &[PathBuf],
) -> io::Result<()> {
    for (entry, source) in entries.iter().zip(sources) {
        if entry.dir {
            continue;
        }
        let meta = tokio::fs::symlink_metadata(source).await?;
        if !meta.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a copied file was replaced by a non-regular file",
            ));
        }
        let file = tokio::fs::File::open(source).await?;
        let mut data = BufReader::with_capacity(COPY_BUF, file.take(entry.size));
        let copied = tokio::io::copy_buf(&mut data, out).await?;
        if copied != entry.size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "a file shrank while it was being sent",
            ));
        }
    }
    out.flush().await
}

/// `name (n).ext`, or `name (n)` without an extension.
fn with_suffix(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// Device names Windows reserves, with or without an extension.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3",
    "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// A name the local file system accepts. `windows` selects the Windows
/// rules (they are also safe on macOS, but only applied where needed).
pub fn local_name(name: &str, windows: bool) -> String {
    if !windows {
        return name.to_string();
    }
    let mut out: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c < ' ' {
                '_'
            } else {
                c
            }
        })
        .collect();
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    if out.is_empty() {
        out.push('_');
    }
    // Windows ignores spaces before the extension when it matches device
    // names ("NUL .txt" is NUL).
    let stem = out
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if WINDOWS_RESERVED.contains(&stem.as_str()) {
        out.insert(0, '_');
    }
    out
}

/// Maps relative transfer paths to unique local paths under a root.
struct NameMapper {
    root: PathBuf,
    windows: bool,
    mapped: HashMap<String, PathBuf>,
    /// (parent, lowercased name) already used.
    taken: HashSet<(PathBuf, String)>,
}

impl NameMapper {
    fn new(root: &Path, windows: bool) -> NameMapper {
        NameMapper {
            root: root.to_path_buf(),
            windows,
            mapped: HashMap::new(),
            taken: HashSet::new(),
        }
    }

    fn map(&mut self, rel: &str) -> PathBuf {
        if let Some(p) = self.mapped.get(rel) {
            return p.clone();
        }
        let (parent, name) = match rel.rsplit_once('/') {
            Some((parent, name)) => (self.map(parent), name),
            None => (self.root.clone(), rel),
        };
        self.claim(rel, parent, &local_name(name, self.windows), 1)
    }

    /// Another name for `rel`, after the file system refused the mapped one
    /// (it folds names together by Unicode case or normalization rules the
    /// mapper does not know).
    fn bump(&mut self, rel: &str) -> PathBuf {
        let parent = self
            .mapped
            .get(rel)
            .and_then(|p| p.parent())
            .map_or_else(|| self.root.clone(), Path::to_path_buf);
        let name = rel.rsplit('/').next().unwrap_or(rel);
        self.claim(rel, parent, &local_name(name, self.windows), 2)
    }

    /// The first free `base`, `base (n)`, ... from the `first`-th, for `rel`.
    fn claim(&mut self, rel: &str, parent: PathBuf, base: &str, first: u32) -> PathBuf {
        let mut n = first;
        let local = loop {
            let candidate = if n == 1 {
                base.to_string()
            } else {
                with_suffix(base, n)
            };
            n += 1;
            if self
                .taken
                .insert((parent.clone(), candidate.to_lowercase()))
            {
                break parent.join(candidate);
            }
        };
        self.mapped.insert(rel.to_string(), local.clone());
        local
    }
}

fn set_metadata(file: &std::fs::File, entry: &FileEntry) {
    let when = u64::try_from(entry.mtime_ms)
        .ok()
        .filter(|ms| *ms > 0)
        .and_then(|ms| UNIX_EPOCH.checked_add(Duration::from_millis(ms)));
    if let Some(when) = when {
        let latest = SystemTime::now() + Duration::from_secs(86_400);
        let _ = file.set_modified(when.min(latest));
    }
    #[cfg(unix)]
    if entry.executable {
        use std::os::unix::fs::PermissionsExt;
        let _ = file.set_permissions(std::fs::Permissions::from_mode(0o755));
    }
}

/// Writes a transfer into `dir` (which must be empty) and returns the
/// local paths of its top-level items, in order. Fails on any short read,
/// extra data or I/O error; the caller then discards `dir`.
pub async fn receive_into(incoming: IncomingFiles, dir: &Path) -> io::Result<Vec<PathBuf>> {
    let IncomingFiles { header, mut body } = incoming;
    receive_body(&header, &mut body, dir, cfg!(windows)).await
}

async fn receive_body(
    header: &FilesHeader,
    body: &mut (impl AsyncRead + Unpin),
    dir: &Path,
    windows: bool,
) -> io::Result<Vec<PathBuf>> {
    let mut body = BufReader::with_capacity(COPY_BUF, body);
    let mut mapper = NameMapper::new(dir, windows);
    let mut tops = Vec::new();
    for entry in &header.entries {
        let mut local = mapper.map(&entry.path);
        if entry.dir {
            tokio::fs::create_dir_all(&local).await?;
        } else {
            if let Some(parent) = local.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let mut renames = 0;
            let mut file = loop {
                let open = tokio::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&local)
                    .await;
                match open {
                    Ok(file) => break file,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists && renames < MAX_RENAMES => {
                        renames += 1;
                        local = mapper.bump(&entry.path);
                    }
                    Err(e) => return Err(e),
                }
            };
            let copied = tokio::io::copy_buf(&mut (&mut body).take(entry.size), &mut file).await?;
            if copied != entry.size {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the transfer ended early",
                ));
            }
            file.flush().await?;
            let file = file.into_std().await;
            set_metadata(&file, entry);
        }
        // Paths are unique in a header, so each top-level item comes once.
        if !entry.path.contains('/') {
            tops.push(local);
        }
    }
    let mut extra = [0u8; 1];
    if body.read(&mut extra).await? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "more data than the header announced",
        ));
    }
    Ok(tops)
}

/// The last transfer number handed out by this process.
static LAST_ID: AtomicU64 = AtomicU64::new(0);

/// A number above every earlier one (and, normally, above those of
/// earlier runs: it starts at the current time in milliseconds).
fn next_id() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let next = |last: u64| now.max(last.saturating_add(1));
    match LAST_ID.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
        Some(next(last))
    }) {
        Ok(last) | Err(last) => next(last),
    }
}

fn id_of(path: &Path) -> Option<u64> {
    path.file_name()?.to_str()?.parse().ok()
}

/// Where received transfers wait until they are pasted: one directory per
/// transfer, numbered in arrival order. A transfer stays until a newer one
/// is on the clipboard, so pasting keeps working across reconnects; failed
/// and overtaken transfers are removed right away.
#[derive(Debug, Clone)]
pub struct Staging {
    root: PathBuf,
}

impl Staging {
    pub fn new(root: impl Into<PathBuf>) -> Staging {
        Staging { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory of a new transfer, numbered above every earlier one;
    /// [`Staging::create`] makes it.
    pub fn reserve(&self) -> PathBuf {
        self.root.join(next_id().to_string())
    }

    /// Creates a reserved directory, which must not exist yet.
    pub async fn create(&self, dir: &Path) -> io::Result<()> {
        tokio::fs::create_dir_all(&self.root).await?;
        tokio::fs::create_dir(dir).await
    }

    /// Removes every transfer older than `dir`, the one now on the
    /// clipboard, and anything else that is not a transfer. Newer
    /// transfers, still arriving, stay.
    pub async fn keep_only(&self, dir: &Path) -> io::Result<()> {
        let Some(keep) = id_of(dir) else {
            return Ok(());
        };
        let mut read = match tokio::fs::read_dir(&self.root).await {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        while let Some(entry) = read.next_entry().await? {
            let path = entry.path();
            if id_of(&path).is_some_and(|id| id >= keep) {
                continue;
            }
            let removed = if entry.file_type().await?.is_dir() {
                tokio::fs::remove_dir_all(&path).await
            } else {
                tokio::fs::remove_file(&path).await
            };
            if let Err(e) = removed {
                warn!(error = %e, "cannot remove an old transfer");
            }
        }
        Ok(())
    }

    /// Removes one transfer (if it exists).
    pub async fn discard(dir: &Path) {
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => warn!(error = %e, "cannot remove a discarded transfer"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        let src = t.path().join("src");
        fs::create_dir_all(src.join("Папка/sub")).unwrap();
        fs::write(src.join("Папка/a.txt"), b"hello").unwrap();
        fs::write(src.join("Папка/sub/b.bin"), vec![7u8; 70_000]).unwrap();
        fs::write(src.join("single.txt"), b"x").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(src.join("single.txt"), src.join("Папка/link")).unwrap();
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(src.join("single.txt"), fs::Permissions::from_mode(0o755)).unwrap();
        }
        t
    }

    #[test]
    fn manifests_walk_folders_skip_symlinks_and_respect_the_limit() {
        let t = tree();
        let src = t.path().join("src");
        let roots = vec![src.join("Папка"), src.join("single.txt")];
        let files = build_manifest(&roots, 1 << 20).unwrap();
        let paths: Vec<&str> = files.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "Папка",
                "Папка/a.txt",
                "Папка/sub",
                "Папка/sub/b.bin",
                "single.txt"
            ]
        );
        assert_eq!(files.total_bytes(), 5 + 70_000 + 1);
        assert_eq!(files.sources[3], src.join("Папка/sub/b.bin"));
        assert!(files.entries[0].dir && files.entries[0].size == 0);
        #[cfg(unix)]
        assert!(files.entries[4].executable);
        assert!(files.entries[1].mtime_ms > 0);
        assert_eq!(
            build_manifest(&roots, 1000),
            Err(ManifestError::TooLarge { bytes: 70_005 })
        );
        assert_eq!(build_manifest(&[], 10), Err(ManifestError::Empty));
    }

    #[test]
    fn same_named_roots_get_unique_names() {
        let t = tempfile::tempdir().unwrap();
        for d in ["a", "b"] {
            fs::create_dir_all(t.path().join(d)).unwrap();
            fs::write(t.path().join(d).join("x.txt"), d).unwrap();
        }
        let roots = vec![t.path().join("a/x.txt"), t.path().join("b/x.txt")];
        let files = build_manifest(&roots, 100).unwrap();
        let paths: Vec<&str> = files.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["x.txt", "x (2).txt"]);
    }

    #[test]
    fn windows_names_are_mapped() {
        assert_eq!(local_name("a:b?.txt", true), "a_b_.txt");
        assert_eq!(local_name("con.txt", true), "_con.txt");
        assert_eq!(local_name("LPT1", true), "_LPT1");
        assert_eq!(local_name("name. ", true), "name");
        assert_eq!(local_name("...", true), "_");
        assert_eq!(local_name("a:b", false), "a:b");
        assert_eq!(local_name("console.log", true), "console.log");
        assert_eq!(local_name("NUL .txt", true), "_NUL .txt");
        assert_eq!(local_name("com¹.txt", true), "_com¹.txt");
        assert_eq!(local_name("conout$", true), "_conout$");
        assert_eq!(local_name("COM10.txt", true), "COM10.txt");
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_that_cannot_travel_whole_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("d");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("ok.txt"), b"1").unwrap();
        fs::write(dir.join("back\\slash"), b"2").unwrap();
        assert!(matches!(
            build_manifest(&[dir], 1 << 20),
            Err(ManifestError::Unsendable(_))
        ));
        let mut deep = t.path().join("deep");
        for _ in 0..MAX_FILE_DEPTH {
            deep = deep.join("x");
        }
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(
            build_manifest(&[t.path().join("deep")], 1 << 20),
            Err(ManifestError::TooMany)
        );
        assert!(matches!(
            build_manifest(&[t.path().join("missing")], 1 << 20),
            Err(ManifestError::Unsendable(_))
        ));
        assert!(matches!(
            build_manifest(&[PathBuf::from("/")], 1 << 20),
            Err(ManifestError::Unsendable(_))
        ));
    }

    fn file_entry(path: &str, size: u64, mtime_ms: i64) -> FileEntry {
        FileEntry {
            path: path.into(),
            size,
            dir: false,
            executable: false,
            mtime_ms,
        }
    }

    #[tokio::test]
    async fn names_the_file_system_folds_together_get_another_name() {
        let t = tempfile::tempdir().unwrap();
        // Stands for a name the mapper did not see as equal (Unicode case
        // or normalization): the file is already there.
        fs::write(t.path().join("a.txt"), b"old").unwrap();
        let header = FilesHeader {
            clock: 1,
            entries: vec![file_entry("a.txt", 3, 0)],
        };
        let mut body: &[u8] = b"new";
        let tops = receive_body(&header, &mut body, t.path(), false)
            .await
            .unwrap();
        assert_eq!(tops, vec![t.path().join("a (2).txt")]);
        assert_eq!(fs::read(t.path().join("a (2).txt")).unwrap(), b"new");
        assert_eq!(fs::read(t.path().join("a.txt")).unwrap(), b"old");
    }

    #[tokio::test]
    async fn absurd_modification_times_are_ignored() {
        let t = tempfile::tempdir().unwrap();
        let header = FilesHeader {
            clock: 1,
            entries: vec![
                file_entry("max", 1, i64::MAX),
                file_entry("min", 1, i64::MIN),
            ],
        };
        let mut body: &[u8] = b"ab";
        receive_body(&header, &mut body, t.path(), true)
            .await
            .unwrap();
        assert!(t.path().join("max").exists() && t.path().join("min").exists());
    }

    #[test]
    fn mapper_keeps_names_unique_case_insensitively() {
        let root = Path::new("/r");
        let mut m = NameMapper::new(root, true);
        assert_eq!(m.map("A.txt"), root.join("A.txt"));
        assert_eq!(m.map("a.txt"), root.join("a (2).txt"));
        assert_eq!(m.map("d/x:y"), root.join("d").join("x_y"));
        assert_eq!(m.map("d/x_y"), root.join("d").join("x_y (2)"));
        assert_eq!(m.map("A.txt"), root.join("A.txt"), "stable");
    }

    async fn round_trip(windows: bool) -> (tempfile::TempDir, Vec<PathBuf>) {
        let t = tree();
        let src = t.path().join("src");
        let files = build_manifest(&[src.join("Папка"), src.join("single.txt")], 1 << 20).unwrap();
        let header = FilesHeader {
            clock: 1,
            entries: files.entries.clone(),
        };
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        let sender = async {
            send_data(&mut writer, &files.entries, &files.sources)
                .await
                .unwrap();
            writer.shutdown().await.unwrap();
        };
        let dest = t.path().join("dest");
        fs::create_dir_all(&dest).unwrap();
        let receiver = receive_body(&header, &mut reader, &dest, windows);
        let ((), tops) = tokio::join!(sender, receiver);
        (t, tops.unwrap())
    }

    #[tokio::test]
    async fn data_round_trips_with_names_sizes_and_times() {
        let (t, tops) = round_trip(false).await;
        let dest = t.path().join("dest");
        assert_eq!(tops, vec![dest.join("Папка"), dest.join("single.txt")]);
        assert_eq!(fs::read(dest.join("Папка/a.txt")).unwrap(), b"hello");
        assert_eq!(
            fs::read(dest.join("Папка/sub/b.bin")).unwrap(),
            vec![7u8; 70_000]
        );
        assert!(!dest.join("Папка/link").exists(), "symlinks are not sent");
        let src_time = fs::metadata(t.path().join("src/Папка/a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let dst_time = fs::metadata(dest.join("Папка/a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let skew = src_time
            .duration_since(dst_time)
            .unwrap_or_else(|e| e.duration());
        assert!(skew < Duration::from_millis(2), "mtime kept: {skew:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dest.join("single.txt"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }
    }

    #[tokio::test]
    async fn short_and_long_bodies_are_errors() {
        let t = tempfile::tempdir().unwrap();
        let header = FilesHeader {
            clock: 1,
            entries: vec![FileEntry {
                path: "f".into(),
                size: 10,
                dir: false,
                executable: false,
                mtime_ms: 0,
            }],
        };
        let mut short: &[u8] = b"12345";
        let d1 = t.path().join("1");
        fs::create_dir_all(&d1).unwrap();
        let e = receive_body(&header, &mut short, &d1, false)
            .await
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
        let mut long: &[u8] = b"0123456789extra";
        let d2 = t.path().join("2");
        fs::create_dir_all(&d2).unwrap();
        let e = receive_body(&header, &mut long, &d2, false)
            .await
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn staging_keeps_the_pasted_transfer_and_newer_ones() {
        let t = tempfile::tempdir().unwrap();
        let staging = Staging::new(t.path().join("incoming"));
        assert_eq!(staging.root(), t.path().join("incoming"));
        let dirs: Vec<PathBuf> = (0..3).map(|_| staging.reserve()).collect();
        assert!(dirs.windows(2).all(|w| id_of(&w[0]) < id_of(&w[1])));
        for d in &dirs {
            staging.create(d).await.unwrap();
            fs::write(d.join("f"), b"1").unwrap();
        }
        assert!(staging.create(&dirs[0]).await.is_err(), "already there");
        fs::write(staging.root().join("junk"), b"").unwrap();
        // The middle transfer is on the clipboard; the newest still arrives.
        staging.keep_only(&dirs[1]).await.unwrap();
        assert!(!dirs[0].exists() && dirs[1].exists() && dirs[2].exists());
        assert!(!staging.root().join("junk").exists());
        Staging::discard(&dirs[2]).await;
        Staging::discard(&dirs[2]).await;
        assert!(!dirs[2].exists());
        Staging::new(t.path().join("none"))
            .keep_only(&dirs[1])
            .await
            .unwrap();
    }
}
