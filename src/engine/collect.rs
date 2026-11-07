use anyhow::{Context, Result, anyhow};
use colored::*;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

use super::cancel::{CancelToken, FirstError};
use super::types::{FileInfo, FileSnapshot, RunMetrics};
use super::types::{emit_tui_event, progress_due};
use crate::logger;
use crate::protocol::{Counts, Phase, TuiEvent, WarningSummary};

/// Names every walk leaves out, with everything under them: a folder a NAS
/// keeps for itself is never read, so a recycle-bin copy never becomes a link
/// target. Each pattern is matched against one file or folder name, with `*`
/// for any run of characters and `?` for one.
#[derive(Clone, Debug, Default)]
pub(super) struct Exclude {
    patterns: Vec<Pattern>,
}

/// One exclude pattern, with the literal text before its first wildcard and
/// after its last, so most names are turned away by a cheap comparison: the
/// walk asks about every name it meets.
#[derive(Clone, Debug)]
struct Pattern {
    text: String,
    prefix: String,
    suffix: String,
    wild: bool,
}

impl Pattern {
    fn new(text: String) -> Self {
        let is_wild = |c: char| c == '*' || c == '?';
        let prefix: String = text.chars().take_while(|&c| !is_wild(c)).collect();
        let wild = prefix.len() < text.len();
        let suffix = if wild {
            let mut tail: Vec<char> = text.chars().rev().take_while(|&c| !is_wild(c)).collect();
            tail.reverse();
            tail.into_iter().collect()
        } else {
            String::new()
        };
        Pattern {
            text,
            prefix,
            suffix,
            wild,
        }
    }

    fn matches(&self, name: &str) -> bool {
        if !self.wild {
            return name == self.text;
        }
        name.len() >= self.prefix.len() + self.suffix.len()
            && name.starts_with(&self.prefix)
            && name.ends_with(&self.suffix)
            && glob_matches(&self.text, name)
    }
}

impl Exclude {
    pub(super) fn new(patterns: Vec<String>) -> Self {
        Exclude {
            patterns: patterns.into_iter().map(Pattern::new).collect(),
        }
    }

    /// The pattern `name` matches, if any.
    pub(super) fn matching(&self, name: &OsStr) -> Option<&str> {
        if self.patterns.is_empty() {
            return None;
        }
        let name = name.to_string_lossy();
        self.patterns
            .iter()
            .find(|pattern| pattern.matches(&name))
            .map(|pattern| pattern.text.as_str())
    }

    /// Whether a walk takes in `path`, logging what it leaves out.
    fn keeps(&self, path: &Path, name: &OsStr) -> bool {
        match self.matching(name) {
            Some(pattern) => {
                logger::info(
                    "scan",
                    format!(
                        "left out {}: its name matches the exclude pattern {pattern:?}",
                        path.display()
                    ),
                );
                false
            }
            None => true,
        }
    }
}

/// Whether `name` matches `pattern`, where `*` stands for any run of
/// characters, `?` for any one, and everything else for itself.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let (mut p, mut n) = (0, 0);
    // Where the last `*` was, and where in `name` it is tried to end.
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((star_p, star_n)) = star {
            // Let the last `*` take one more character and try again.
            p = star_p + 1;
            n = star_n + 1;
            star = Some((star_p, star_n + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[derive(Copy, Clone)]
pub(super) struct CollectOptions<'a> {
    /// Files smaller than this are skipped; at least 1, so empty files always are.
    pub min_size: u64,
    pub exclude: &'a Exclude,
    pub strict: bool,
    pub show_progress: bool,
    pub tui_events: bool,
    pub cancel: &'a CancelToken,
}

/// DejaView writes its cache, reports and temp files under the `.dejaview_`
/// prefix in the downloads root; they are never candidates.
pub(super) fn is_own_file(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with(".dejaview_"))
}

/// A root folder as the filesystem knows it: its device and inode, which stay
/// the same through every mount that shows it.
#[derive(Clone, Debug)]
pub(super) struct RootId {
    pub(super) name: &'static str,
    pub(super) path: PathBuf,
    /// The path with symlinks resolved, or the path itself if it cannot be.
    pub(super) canonical: PathBuf,
    pub(super) dev: u64,
    pub(super) ino: u64,
}

impl RootId {
    pub(super) fn of(name: &'static str, path: &Path) -> Result<Self> {
        let meta = fs::metadata(path)
            .with_context(|| format!("cannot read the {name} folder {}", path.display()))?;
        Ok(RootId {
            name,
            path: path.to_path_buf(),
            canonical: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.path) || path.starts_with(&self.canonical)
    }
}

/// `base` joined with `rel`, with `.` and `..` worked out on the text alone:
/// a dangling symlink's destination cannot be resolved on disk.
fn lexical_join(base: &Path, rel: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in base.join(rel).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A symlink in downloads that points into a library folder and no longer
/// resolves: most likely a link DejaView made whose library file Sonarr or
/// Radarr has since upgraded, renamed or deleted, so its torrent cannot seed.
fn is_dangling_link_into(link: &Path, library: &[&RootId]) -> bool {
    let Ok(dest) = fs::read_link(link) else {
        return false;
    };
    let parent = link.parent().unwrap_or(Path::new("/"));
    let dest = lexical_join(parent, &dest);
    library.iter().any(|folder| folder.contains(&dest))
        && matches!(fs::metadata(link), Err(err) if err.kind() == std::io::ErrorKind::NotFound)
}

/// Refuse a directory that is another root seen through a second mount.
/// Path checks cannot see a bind mount, so the walk compares device and inode:
/// without this, a library reached from inside downloads is walked as
/// downloads and its files get rewritten.
pub(super) fn ensure_not_other_root(
    dir: &Path,
    walking: &RootId,
    others: &[&RootId],
) -> Result<()> {
    let meta = match fs::metadata(dir) {
        Ok(meta) => meta,
        // An unreadable directory is the walk's own error to report.
        Err(_) => return Ok(()),
    };
    if let Some(other) = others
        .iter()
        .find(|other| meta.dev() == other.dev && meta.ino() == other.ino)
    {
        if walking.name == "media" && other.name == "media" {
            return Err(anyhow!(
                "the library folder {} reaches the library folder {} at {}: the same directory through a second mount, such as a bind mount, so its files would be counted twice. List only one of them, or point them at folders that do not contain each other on disk.",
                walking.path.display(),
                other.path.display(),
                dir.display()
            ));
        }
        return Err(anyhow!(
            "the {} folder {} reaches the {} folder {} at {}: the same directory through a second mount, such as a bind mount. DejaView would treat library files as downloads and replace them. Point downloads and media at separate folders that neither contains the other on disk.",
            walking.name,
            walking.path.display(),
            other.name,
            other.path.display(),
            dir.display()
        ));
    }
    Ok(())
}

/// Walk only the directories of `walking` and refuse if one of them is one of
/// `others`. `config validate` runs this from every root; a scan does the
/// same check as part of its own walk.
pub(super) fn ensure_root_not_reached(
    walking: &RootId,
    others: &[&RootId],
    exclude: &Exclude,
) -> Result<()> {
    for entry in WalkDir::new(&walking.path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || exclude.keeps(entry.path(), entry.file_name()))
    {
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_dir() {
            ensure_not_other_root(entry.path(), walking, others)?;
        }
    }
    Ok(())
}

/// Every root of a run but `root`: downloads and the other library folders,
/// which no folder of `root` may be.
pub(super) fn other_roots<'a>(
    root: &RootId,
    downloads: &'a RootId,
    library: &'a [RootId],
) -> Vec<&'a RootId> {
    std::iter::once(downloads)
        .chain(library.iter())
        .filter(|other| !std::ptr::eq(*other, root))
        .collect()
}

/// What the walk of one root found. Each folder fills its own and merges it
/// once, so workers do not contend for a lock per file.
#[derive(Default)]
struct Found {
    files: Vec<FileInfo>,
    walk_errors: u64,
    metadata_errors: u64,
    skipped_by_min_size: u64,
    skipped_by_min_age: u64,
    skipped_by_exclude: u64,
    dangling_symlinks: Vec<PathBuf>,
    unreadable: Vec<PathBuf>,
}

impl Found {
    fn merge(&mut self, other: Found) {
        self.files.extend(other.files);
        self.walk_errors += other.walk_errors;
        self.metadata_errors += other.metadata_errors;
        self.skipped_by_min_size += other.skipped_by_min_size;
        self.skipped_by_min_age += other.skipped_by_min_age;
        self.skipped_by_exclude += other.skipped_by_exclude;
        self.dangling_symlinks.extend(other.dangling_symlinks);
        self.unreadable.extend(other.unreadable);
    }

    /// Hand the counts to the run and return the files, sorted by path so a
    /// run's order does not depend on which worker got where first.
    fn finish(
        mut self,
        in_downloads: bool,
        warnings: &mut WarningSummary,
        metrics: &mut RunMetrics,
    ) -> Vec<FileInfo> {
        warnings.walk_errors += self.walk_errors;
        warnings.metadata_errors += self.metadata_errors;
        metrics.skipped_by_min_size += self.skipped_by_min_size;
        metrics.skipped_by_min_age += self.skipped_by_min_age;
        metrics.skipped_by_exclude += self.skipped_by_exclude;
        self.dangling_symlinks.sort();
        metrics.dangling_symlinks.extend(self.dangling_symlinks);
        self.unreadable.sort();
        metrics
            .unreadable
            .extend(self.unreadable.into_iter().map(|path| (path, in_downloads)));
        self.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.files
    }
}

/// What every worker of a run's walks shares: the first error that stops
/// them all (another root reached, a strict failure, a cancel), and the count
/// of files found so far, for the TUI.
#[derive(Default)]
struct WalkShared {
    stop: FirstError,
    files_seen: AtomicUsize,
}

/// The walk of one root, run by many workers at once: each folder is read by
/// one worker, which hands the folders it finds back to the pool. On a
/// network drive a walk waits on one `stat` after another, so the pool is
/// sized for how many the drive answers at once, not for the CPU.
struct Walk<'a> {
    root: &'a RootId,
    /// Every other root of the run, which no folder of this one may be.
    others: Vec<&'a RootId>,
    in_downloads: bool,
    deadline: SystemTime,
    options: &'a CollectOptions<'a>,
    found: Mutex<Found>,
}

impl<'a> Walk<'a> {
    fn new(
        root: &'a RootId,
        others: Vec<&'a RootId>,
        in_downloads: bool,
        min_age: Duration,
        options: &'a CollectOptions<'a>,
    ) -> Self {
        Walk {
            root,
            others,
            in_downloads,
            deadline: SystemTime::now() - min_age,
            options,
            found: Mutex::new(Found::default()),
        }
    }

    fn walk_error(&self, shared: &WalkShared, found: &mut Found, path: &Path, err: std::io::Error) {
        found.walk_errors += 1;
        found.unreadable.push(path.to_path_buf());
        logger::warn(
            "scan",
            format!(
                "cannot read {}: {err}; what it holds is left out of this run. Make it readable, or add its name to the exclude list (`dejaview config set-exclude`)",
                path.display()
            ),
        );
        if self.options.strict {
            shared.stop.set(anyhow!(
                "walk error under {}: cannot read {}: {err}. Make it readable, add its name to the exclude list (`dejaview config set-exclude`), or run without --strict, which skips it with a warning",
                self.root.path.display(),
                path.display()
            ));
        }
    }

    /// Read one folder: hand its folders to the pool and look at its files.
    fn read_folder<'s>(
        &'s self,
        scope: &rayon::Scope<'s>,
        shared: &'s WalkShared,
        dir: &Path,
        found: &mut Found,
    ) {
        if shared.stop.should_stop(self.options.cancel) {
            return;
        }
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => return self.walk_error(shared, found, dir, err),
        };
        for entry in entries {
            if shared.stop.should_stop(self.options.cancel) {
                return;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    self.walk_error(shared, found, dir, err);
                    continue;
                }
            };
            let path = entry.path();
            let name = entry.file_name();
            if !self.options.exclude.keeps(&path, &name) {
                found.skipped_by_exclude += 1;
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    self.walk_error(shared, found, &path, err);
                    continue;
                }
            };
            if file_type.is_dir() {
                if let Err(err) = ensure_not_other_root(&path, self.root, &self.others) {
                    shared.stop.set(err);
                    return;
                }
                scope.spawn(move |scope| self.walk_folder(scope, shared, path));
            } else if file_type.is_symlink() {
                if self.in_downloads && is_dangling_link_into(&path, &self.others) {
                    found.dangling_symlinks.push(path);
                }
            } else if file_type.is_file() && !is_own_file(&name) {
                self.look_at_file(shared, found, path, &entry);
            }
        }
    }

    fn walk_folder<'s>(&'s self, scope: &rayon::Scope<'s>, shared: &'s WalkShared, dir: PathBuf) {
        let mut found = Found::default();
        self.read_folder(scope, shared, &dir, &mut found);
        if let Ok(mut all) = self.found.lock() {
            all.merge(found);
        }
    }

    fn look_at_file(
        &self,
        shared: &WalkShared,
        found: &mut Found,
        path: PathBuf,
        entry: &fs::DirEntry,
    ) {
        // DirEntry::metadata does not follow a symlink, and the walk already
        // knows this is a regular file.
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(err) => {
                found.metadata_errors += 1;
                found.unreadable.push(path.clone());
                logger::warn(
                    "scan",
                    format!(
                        "cannot read the metadata of {}: {err}; it is left out of this run. Make it readable, or add its name to the exclude list (`dejaview config set-exclude`)",
                        path.display()
                    ),
                );
                if self.options.strict {
                    shared
                        .stop
                        .set(anyhow!(
                            "metadata error for {}: {err}. Make it readable, or run without --strict, which skips it with a warning",
                            path.display()
                        ));
                }
                return;
            }
        };
        if meta.len() < self.options.min_size.max(1) {
            found.skipped_by_min_size += 1;
            return;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if mtime > self.deadline {
            found.skipped_by_min_age += 1;
            return;
        }
        if self.options.tui_events {
            let seen = shared.files_seen.fetch_add(1, Ordering::Relaxed) + 1;
            if progress_due() {
                let _ = emit_tui_event(&TuiEvent::File {
                    phase: Some(Phase::Collecting),
                    path: path.display().to_string(),
                });
                let _ = emit_tui_event(&TuiEvent::Count(Counts {
                    files_seen: Some(seen),
                    ..Counts::default()
                }));
            }
        }
        found.files.push(FileInfo {
            path,
            snapshot: FileSnapshot::from_metadata(&meta),
            hash: None,
            sample: None,
            in_downloads: self.in_downloads,
        });
    }
}

/// Walk every root at once on the current rayon pool, and return each one's
/// files in the order given.
fn walk_roots(
    walks: &[Walk<'_>],
    warnings: &mut WarningSummary,
    metrics: &mut RunMetrics,
) -> Result<Vec<Vec<FileInfo>>> {
    let shared = WalkShared::default();
    for walk in walks {
        ensure_not_other_root(&walk.root.path, walk.root, &walk.others)?;
    }
    rayon::scope(|scope| {
        for walk in walks {
            let shared = &shared;
            scope.spawn(move |scope| walk.walk_folder(scope, shared, walk.root.path.clone()));
        }
    });
    // The counts go to the run even when the walk stopped on an error.
    let files = walks
        .iter()
        .map(|walk| {
            let found = walk
                .found
                .lock()
                .map(|mut found| std::mem::take(&mut *found))
                .unwrap_or_default();
            found.finish(walk.in_downloads, warnings, metrics)
        })
        .collect();
    shared.stop.into_result()?;
    Ok(files)
}

/// Walk one root. A run walks both at once with [`collect_run_files`].
#[cfg(test)]
pub(super) fn collect_files(
    root: &RootId,
    other_root: &RootId,
    in_downloads: bool,
    min_age: Duration,
    options: &CollectOptions<'_>,
    warnings: &mut WarningSummary,
    metrics: &mut RunMetrics,
) -> Result<Vec<FileInfo>> {
    let walks = [Walk::new(
        root,
        vec![other_root],
        in_downloads,
        min_age,
        options,
    )];
    let mut files = walk_roots(&walks, warnings, metrics)?;
    Ok(files.pop().unwrap_or_default())
}

/// Walk downloads and every library folder at once, on `pool`.
pub(super) fn collect_run_files<P: AsRef<Path>>(
    downloads: &Path,
    media: &[P],
    min_age: Duration,
    options: CollectOptions<'_>,
    pool: &rayon::ThreadPool,
    warnings: &mut WarningSummary,
    metrics: &mut RunMetrics,
) -> Result<Vec<FileInfo>> {
    let spinner = if options.show_progress {
        let mp = MultiProgress::new();
        let spinner = mp.add(ProgressBar::new_spinner());
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner} {msg}")
                .unwrap(),
        );
        spinner.enable_steady_tick(Duration::from_millis(120));
        spinner.set_message("Collecting files…".cyan().bold().to_string());
        Some(spinner)
    } else {
        None
    };

    let downloads_root = RootId::of("downloads", downloads)?;
    let library = media
        .iter()
        .map(|folder| RootId::of("media", folder.as_ref()))
        .collect::<Result<Vec<_>>>()?;
    let mut walks = vec![Walk::new(
        &downloads_root,
        other_roots(&downloads_root, &downloads_root, &library),
        true,
        min_age,
        &options,
    )];
    for folder in &library {
        let others = other_roots(folder, &downloads_root, &library);
        walks.push(Walk::new(folder, others, false, min_age, &options));
    }
    let files = pool.install(|| walk_roots(&walks, warnings, metrics))?;

    if let Some(spinner) = spinner {
        spinner.finish_with_message("File list complete ✔".green().bold().to_string());
    }

    Ok(files.into_iter().flatten().collect())
}

pub(super) fn group_files_by_size(files: Vec<FileInfo>) -> Vec<Vec<FileInfo>> {
    let mut size_map: HashMap<u64, Vec<FileInfo>> = HashMap::new();
    for f in files {
        size_map.entry(f.snapshot.size).or_default().push(f);
    }
    size_map.into_values().filter(|v| v.len() > 1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_names() {
        assert!(glob_matches("@eaDir", "@eaDir"));
        assert!(!glob_matches("@eaDir", "@eaDir2"));
        assert!(!glob_matches("@eaDir", "x@eaDir"));
        assert!(glob_matches(".Trash-*", ".Trash-1000"));
        assert!(glob_matches(".Trash-*", ".Trash-"));
        assert!(!glob_matches(".Trash-*", ".Trash"));
        assert!(glob_matches("*.part", "movie.mkv.part"));
        assert!(!glob_matches("*.part", "movie.part.mkv"));
        assert!(glob_matches("a*b*c", "aXXbYYbZc"));
        assert!(!glob_matches("a*b*c", "aXXbYY"));
        assert!(glob_matches("?.nfo", "x.nfo"));
        assert!(!glob_matches("?.nfo", "xy.nfo"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("$RECYCLE.BIN", "$RECYCLE.BIN"));
    }

    #[test]
    fn a_pattern_matches_as_the_glob_does() {
        let names = [
            ".Trash-1000",
            ".Trash-",
            ".Trash",
            "movie.mkv.part",
            "x.nfo",
            "xy.nfo",
            "@eaDir",
            "a.b",
            "ab",
            "aXbYc",
            "",
        ];
        for pattern in [
            ".Trash-*", "*.part", "?.nfo", "@eaDir", "a*b*c", "a?b", "*", "a*", "*b",
        ] {
            let compiled = Pattern::new(pattern.to_string());
            for name in names {
                assert_eq!(
                    compiled.matches(name),
                    glob_matches(pattern, name),
                    "{pattern} against {name}"
                );
            }
        }
    }

    #[test]
    fn exclude_names_the_pattern_matched() {
        let exclude = Exclude::new(vec!["#recycle".into(), ".Trash-*".into()]);
        assert_eq!(exclude.matching(OsStr::new(".Trash-0")), Some(".Trash-*"));
        assert_eq!(exclude.matching(OsStr::new("Movies")), None);
    }
}
