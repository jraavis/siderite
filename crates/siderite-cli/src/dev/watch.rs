//! What `dev` watches, and turning filesystem events into rebuilds.
//!
//! The inputs are the files the binary was compiled from (cargo's
//! dep-info file next to the executable lists every local source file,
//! including path dependencies and `include_str!` files), plus the
//! manifests, `Cargo.lock`, `siderite.toml` and the migrations directory.
//! Before the first successful build, `src/` and `build.rs` of the
//! package stand in for the dep-info. Anything under the target directory
//! or a VCS directory is never an input, so files the app writes at run
//! time (databases, logs, uploads) do not cause restart loops.
//!
//! Parent directories are watched non-recursively, which survives editors
//! that save by renaming. A burst of events is debounced into one rebuild
//! request; a watcher overflow or error requests a rebuild instead of
//! losing changes.

use super::Event;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Quiet period that ends a burst of relevant events.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(150);

/// Longest a burst may delay its rebuild.
pub(crate) const MAX_BURST: Duration = Duration::from_secs(1);

/// Directory names that never hold inputs.
const IGNORED_DIRS: [&str; 4] = [".git", ".hg", ".svn", "target"];

type Dirs = BTreeSet<PathBuf>;
/// One notification from the platform watcher.
type Raw = notify::Result<notify::Event>;

/// Where the inputs live.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    /// The application package directory.
    pub package: PathBuf,
    /// The workspace root (the package directory for a lone crate).
    pub root: PathBuf,
    /// Cargo's target directory.
    pub target: PathBuf,
    /// The migrations directory.
    pub migrations: PathBuf,
}

/// The files and directories that trigger a rebuild.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Inputs {
    /// Watched files (canonical where they exist).
    files: BTreeSet<PathBuf>,
    /// Directories whose every direct entry is an input.
    dirs: BTreeSet<PathBuf>,
}

impl Inputs {
    /// Manifests, lockfile, config and migrations, plus `sources`.
    pub(crate) fn new(layout: &Layout, sources: Vec<PathBuf>) -> Self {
        let mut inputs = Self::default();
        for dir in [&layout.package, &layout.root] {
            for name in ["Cargo.toml", "Cargo.lock", "siderite.toml"] {
                inputs.add_file(&dir.join(name), layout);
            }
        }
        inputs.dirs.insert(canonical(&layout.migrations));
        for file in sources {
            inputs.add_file(&file, layout);
        }
        inputs
    }

    /// The inputs before any build: everything under the package's
    /// `src/`, and `build.rs`.
    pub(crate) fn initial(layout: &Layout) -> Self {
        let mut sources = vec![layout.package.join("build.rs")];
        walk(&layout.package.join("src"), &mut sources);
        Self::new(layout, sources)
    }

    /// The inputs of a built binary from its dep-info file `exe.d`.
    pub(crate) fn built(layout: &Layout, exe: &Path) -> Self {
        let text = std::fs::read_to_string(exe.with_extension("d"));
        match text {
            Ok(text) => Self::new(layout, dep_info(&text)),
            Err(_) => Self::initial(layout),
        }
    }

    fn add_file(&mut self, file: &Path, layout: &Layout) {
        if !ignored(file, layout) {
            self.files.insert(canonical(file));
        }
    }

    /// Directories to watch (non-recursively).
    pub(crate) fn watch_dirs(&self) -> BTreeSet<PathBuf> {
        let parents = self.files.iter().filter_map(|f| f.parent());
        let mut dirs: Dirs = parents.map(Path::to_path_buf).collect();
        dirs.extend(self.dirs.iter().cloned());
        dirs
    }

    /// Whether a change to `path` is a change of an input.
    pub(crate) fn matches(&self, path: &Path) -> bool {
        let path = canonical(path);
        let in_dir = path.parent().is_some_and(|d| self.dirs.contains(d));
        self.files.contains(&path) || in_dir
    }
}

/// Paths from a make-style dep-info file (`target: dep dep ...`), with
/// `\ ` escapes and line continuations.
pub(crate) fn dep_info(text: &str) -> Vec<PathBuf> {
    let joined = text.replace("\\\n", " ");
    let Some(line) = joined.lines().next() else {
        return Vec::new();
    };
    let deps = line.split_once(": ").map_or("", |(_, deps)| deps);
    let mut paths = Vec::new();
    let mut current = String::new();
    let mut chars = deps.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => current.extend(chars.next()),
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    paths.push(PathBuf::from(std::mem::take(&mut current)));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        paths.push(PathBuf::from(current));
    }
    paths
}

/// Under the target directory, or under a VCS or `target` directory
/// inside the workspace (directories above the workspace do not count).
fn ignored(path: &Path, layout: &Layout) -> bool {
    let path = canonical(path);
    if path.starts_with(canonical(&layout.target)) {
        return true;
    }
    let root = canonical(&layout.root);
    let inside = path.strip_prefix(&root).unwrap_or(Path::new(""));
    inside.components().any(|c| match c {
        Component::Normal(name) => IGNORED_DIRS.iter().any(|d| name == *d),
        _ => false,
    })
}

/// `path` with links resolved. For a missing path, the nearest existing
/// ancestor is resolved and the missing components are kept as given.
fn canonical(path: &Path) -> PathBuf {
    path.ancestors()
        .find_map(|dir| {
            let found = dir.canonicalize().ok()?;
            let rest = path.strip_prefix(dir).ok()?;
            Some(found.join(rest))
        })
        .unwrap_or_else(|| path.to_path_buf())
}

/// Every `.rs` file under `dir`, skipping ignored directories.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() && !IGNORED_DIRS.iter().any(|d| name == *d) {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether a raw notification is worth a rebuild: a change of an input,
/// or anything the watcher could not report precisely.
pub(crate) fn relevant(event: &Raw, inputs: &Inputs) -> bool {
    use notify::EventKind;
    let Ok(event) = event else {
        // An error (for example a queue overflow): assume a change.
        return true;
    };
    if event.need_rescan() {
        return true;
    }
    let changing = matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Modify(_)
            // Deletions and anything the backend cannot classify.
            | EventKind::Remove(_)
            | EventKind::Any
    );
    changing && event.paths.iter().any(|p| inputs.matches(p))
}

/// Watches [`Inputs`] and sends [`Event::Changed`] after each debounced
/// burst of relevant changes.
pub(crate) struct Watch {
    watcher: RecommendedWatcher,
    watched: BTreeSet<PathBuf>,
    raw: mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
}

impl Watch {
    /// Start a watcher; it watches nothing until [`Self::set`].
    ///
    /// Errors:
    ///   The platform watcher could not be created.
    pub(crate) fn start() -> notify::Result<Self> {
        let (tx, raw) = mpsc::unbounded_channel();
        let watcher = notify::recommended_watcher(move |event| {
            let _ = tx.send(event);
        })?;
        Ok(Self {
            watcher,
            watched: BTreeSet::new(),
            raw,
        })
    }

    /// Watch exactly the directories of `inputs`. Directories that cannot
    /// be watched are reported once and skipped.
    pub(crate) fn set(&mut self, inputs: &Inputs) {
        let wanted = inputs.watch_dirs();
        for gone in self.watched.difference(&wanted) {
            let _ = self.watcher.unwatch(gone);
        }
        let mut watched = BTreeSet::new();
        for dir in wanted {
            if self.watched.contains(&dir) {
                watched.insert(dir);
                continue;
            }
            if !dir.is_dir() {
                continue;
            }
            match self.watcher.watch(&dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched.insert(dir);
                }
                Err(err) => {
                    let shown = dir.display();
                    eprintln!("siderite dev: cannot watch {shown}: {err}");
                }
            }
        }
        self.watched = watched;
    }

    /// Wait for the next relevant change and the quiet period after it.
    /// `None` when the watcher has stopped.
    pub(crate) async fn changed(&mut self, inputs: &Inputs) -> Option<Event> {
        loop {
            let event = self.raw.recv().await?;
            if relevant(&event, inputs) {
                break;
            }
        }
        // Only relevant events extend the quiet period, and never past
        // the cap: a busy, irrelevant file must not starve the rebuild.
        let started = Instant::now();
        let mut quiet = started + DEBOUNCE;
        loop {
            let deadline = quiet.min(started + MAX_BURST);
            match tokio::time::timeout_at(deadline, self.raw.recv()).await {
                Ok(Some(event)) if relevant(&event, inputs) => {
                    quiet = Instant::now() + DEBOUNCE;
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        Some(Event::Changed)
    }
}
