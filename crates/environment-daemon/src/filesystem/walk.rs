//! One traversal for `fs/searchText`, `fs/globFiles`, `fs/scan` and transfer
//! captures: the [`TreeFilter`] pattern grammar and a resumable walk beneath
//! the root descriptor.
//!
//! Patterns match paths relative to the walk's root, gitignore-style: `*`
//! and `?` within one segment, `**` across segments, `[...]` and `{a,b}`. A
//! pattern without `/` matches the entry's name at any depth, a `/` anywhere
//! but at the end anchors it to the root, and a trailing `/` matches
//! directories only. The backslash is an ordinary character, as in names.
//!
//! The walk visits children in byte order of their names, depth first, so
//! results are sorted by path. It always prunes VCS metadata directories and
//! the daemon state directory; by default it respects `.gitignore`,
//! `.ignore` and `.git/info/exclude` (in that ascending precedence, each
//! level overriding its ancestors) and skips names starting with `.`.

use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    sync::Arc,
};

use environment_protocol::{
    data::fs::{FileKind, TreeFilter},
    error::{PermissionReason, PreconditionReason, ProtocolError},
    paths::TreePath,
};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use super::{
    confinement::{Confinement, Resolved},
    io_error,
    os::{self, Directory, EntryStat},
};

/// Directories every walk prunes, whatever the filter says.
const VCS_DIRECTORIES: [&str; 4] = [".git", ".hg", ".svn", ".jj"];
/// Steps (pruned entries, listings) a walk takes without an event before it
/// yields, so its caller can check its deadline and cancellation.
const STEPS_PER_YIELD: u32 = 1024;
/// Largest ignore file read; longer files are cut.
const MAX_IGNORE_FILE_BYTES: u64 = 1024 * 1024;

/// A compiled pattern list.
#[derive(Clone, Debug, Default)]
pub(crate) struct PatternSet {
    any: Option<GlobSet>,
    directories: Option<GlobSet>,
    count: usize,
}

impl PatternSet {
    /// Compile `patterns`; `field` names the list in errors
    /// (`filter.include`).
    pub fn compile(
        patterns: &[String],
        ignore_case: bool,
        field: &str,
    ) -> Result<Self, ProtocolError> {
        let mut any = GlobSetBuilder::new();
        let mut directories = GlobSetBuilder::new();
        let (mut any_count, mut directory_count) = (0, 0);
        for (index, pattern) in patterns.iter().enumerate() {
            let field = format!("{field}[{index}]");
            let (glob, directory_only) = translate(pattern)
                .ok_or_else(|| ProtocolError::invalid_field(&field, "empty pattern"))?;
            let glob = GlobBuilder::new(&glob)
                .literal_separator(true)
                .backslash_escape(false)
                .case_insensitive(ignore_case)
                .build()
                .map_err(|error| {
                    ProtocolError::invalid_field(&field, format!("invalid pattern: {error}"))
                })?;
            if directory_only {
                directories.add(glob);
                directory_count += 1;
            } else {
                any.add(glob);
                any_count += 1;
            }
        }
        let build = |builder: GlobSetBuilder, count: usize| -> Result<_, ProtocolError> {
            if count == 0 {
                return Ok(None);
            }
            builder
                .build()
                .map(Some)
                .map_err(|error| ProtocolError::invalid_field(field, error.to_string()))
        };
        Ok(Self {
            any: build(any, any_count)?,
            directories: build(directories, directory_count)?,
            count: patterns.len(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn matches(&self, path: &TreePath, is_dir: bool) -> bool {
        let path = path.as_str();
        self.any.as_ref().is_some_and(|set| set.is_match(path))
            || (is_dir
                && self
                    .directories
                    .as_ref()
                    .is_some_and(|set| set.is_match(path)))
    }
}

/// Gitignore-style anchoring into a globset pattern: `(glob, directory
/// only)`, or `None` for an empty pattern.
fn translate(pattern: &str) -> Option<(String, bool)> {
    let (body, directory_only) = match pattern.strip_suffix('/') {
        Some(body) if !body.is_empty() => (body, true),
        _ => (pattern, false),
    };
    if body.is_empty() || body == "/" {
        return None;
    }
    let glob = if let Some(anchored) = body.strip_prefix('/') {
        anchored.to_owned()
    } else if body.contains('/') {
        body.to_owned()
    } else {
        format!("**/{body}")
    };
    Some((glob, directory_only))
}

/// A [`TreeFilter`] ready to walk.
#[derive(Clone, Debug)]
pub(crate) struct CompiledFilter {
    pub include: PatternSet,
    pub exclude: PatternSet,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub max_depth: Option<u32>,
}

impl CompiledFilter {
    /// `prefix` is the wire field holding the filter (`filter`).
    pub fn compile(filter: &TreeFilter, prefix: &str) -> Result<Self, ProtocolError> {
        Ok(Self {
            include: PatternSet::compile(
                &filter.include,
                filter.ignore_case,
                &format!("{prefix}.include"),
            )?,
            exclude: PatternSet::compile(
                &filter.exclude,
                filter.ignore_case,
                &format!("{prefix}.exclude"),
            )?,
            include_hidden: filter.include_hidden,
            include_ignored: filter.include_ignored,
            max_depth: filter.max_depth,
        })
    }

    /// Visit everything: no patterns, hidden and ignored entries included.
    pub fn everything() -> Self {
        Self {
            include: PatternSet::default(),
            exclude: PatternSet::default(),
            include_hidden: true,
            include_ignored: true,
            max_depth: None,
        }
    }

    pub fn selects(&self, path: &TreePath, is_dir: bool) -> bool {
        self.include.is_empty() || self.include.matches(path, is_dir)
    }
}

/// What a walk does with a symlink it meets below its root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkMode {
    /// Report the link itself as an entry of kind symlink.
    Report,
    /// Follow it when its target stays inside the filesystem root; report
    /// it as skipped otherwise.
    Follow,
}

/// One step of a walk.
#[derive(Debug)]
pub(crate) enum Event {
    Entry(Entry),
    /// Nothing to report yet: the walk pruned or listed for a while. The
    /// caller checks its bounds and steps again.
    Yield,
    Skip {
        /// Absent when the name is not UTF-8.
        path: Option<TreePath>,
        reason: Skip,
        /// The skipped entry matters to the filter: its path, taken as a
        /// non-directory, matches `include` (always without includes), or
        /// it is a directory the walk had to descend. Always true without a
        /// path.
        selected: bool,
    },
}

/// Why a walk left an entry out.
#[derive(Debug)]
pub(crate) enum Skip {
    NonUtf8Name,
    /// The entry vanished, could not be read, or (a directory) could not be
    /// listed.
    Unreadable(ProtocolError),
    /// A followed link left the root, addressed the state directory, looped
    /// or closed a directory cycle.
    LinkOutsideRoot(ProtocolError),
    /// A followed link whose target does not exist.
    DanglingLink,
}

/// An entry the walk visited.
#[derive(Debug)]
pub(crate) struct Entry {
    /// Relative to the walk's root, as walked.
    pub path: TreePath,
    pub depth: u32,
    /// The directory holding the entry: for a followed link, its target.
    pub parent: Arc<Directory>,
    pub name: OsString,
    /// Canonical segments below the filesystem root.
    pub canonical: Vec<OsString>,
    /// `lstat` of the entry, or of the link's target when it was followed.
    pub stat: EntryStat,
    /// Reached through a followed symlink.
    pub via_link: bool,
    /// Matches the filter's `include` (always true without includes).
    pub selected: bool,
}

struct Frame {
    directory: Arc<Directory>,
    canonical: Vec<OsString>,
    path: TreePath,
    depth: u32,
    identity: (u64, u64),
    /// Children left to visit, in reverse order; `None` until listed.
    names: Option<Vec<OsString>>,
    /// Ignore levels pushed when this frame was listed.
    ignore_levels: usize,
}

struct IgnoreLevel {
    matcher: Gitignore,
}

/// A depth-first walk beneath a resolved directory. Resumable: callers step
/// it with [`Walker::next_event`] and may stop at any time.
pub(crate) struct Walker {
    confinement: Arc<Confinement>,
    filter: CompiledFilter,
    links: LinkMode,
    max_depth: Option<u32>,
    /// Coordinates of ignore matching: the walk root's canonical segments.
    base: PathBuf,
    stack: Vec<Frame>,
    ignores: Vec<IgnoreLevel>,
    identities: HashSet<(u64, u64)>,
}

impl Walker {
    /// Walk below `root`, which must be a resolved directory. `max_depth`
    /// overrides the filter's.
    pub fn new(
        confinement: Arc<Confinement>,
        root: &Resolved,
        filter: CompiledFilter,
        links: LinkMode,
        max_depth: Option<u32>,
        subject: &dyn std::fmt::Display,
    ) -> Result<Self, ProtocolError> {
        let directory = root
            .open_directory()
            .map_err(|error| io_error(error, subject))?;
        let stat = EntryStat::of_directory(&directory).map_err(|error| io_error(error, subject))?;
        let base = segments_path(&root.segments);
        let mut walker = Self {
            confinement,
            max_depth: max_depth.or(filter.max_depth),
            filter,
            links,
            base,
            stack: Vec::new(),
            ignores: Vec::new(),
            identities: HashSet::new(),
        };
        if !walker.filter.include_ignored {
            walker.load_ancestor_ignores(&root.segments);
        }
        walker.identities.insert((stat.dev, stat.ino));
        walker.stack.push(Frame {
            directory: Arc::new(directory),
            canonical: root.segments.clone(),
            path: TreePath::root(),
            depth: 0,
            identity: (stat.dev, stat.ino),
            names: None,
            ignore_levels: 0,
        });
        Ok(walker)
    }

    /// The next entry or skip, [`Event::Yield`] after a stretch without
    /// one, or `None` when the walk is done.
    pub fn next_event(&mut self) -> Option<Event> {
        for _ in 0..STEPS_PER_YIELD {
            let frame = self.stack.last_mut()?;
            if frame.names.is_none() {
                if let Some(event) = self.list_top() {
                    return Some(event);
                }
                continue;
            }
            let frame = self.stack.last_mut().expect("frame listed above");
            let Some(name) = frame.names.as_mut().and_then(Vec::pop) else {
                self.pop_frame();
                continue;
            };
            if let Some(event) = self.visit(name) {
                return Some(event);
            }
        }
        Some(Event::Yield)
    }

    /// List the top frame; an unlistable directory is skipped and popped.
    fn list_top(&mut self) -> Option<Event> {
        let frame = self.stack.last_mut().expect("caller checked");
        if self.max_depth.is_some_and(|max| frame.depth >= max) {
            frame.names = Some(Vec::new());
            return None;
        }
        match frame.directory.names() {
            Ok(mut names) => {
                names.sort_by(|left, right| right.as_bytes().cmp(left.as_bytes()));
                frame.names = Some(names);
                if !self.filter.include_ignored {
                    let directory = frame.directory.clone();
                    let at = self.base.join(segments_path_tree(&frame.path));
                    let pushed = self.load_ignores(&directory, at);
                    self.stack.last_mut().expect("frame").ignore_levels = pushed;
                }
                None
            }
            Err(error) => {
                let path = frame.path.clone();
                let error = io_error(error, &path);
                self.pop_frame();
                Some(Event::Skip {
                    path: (!path.is_root()).then_some(path),
                    reason: Skip::Unreadable(error),
                    selected: true,
                })
            }
        }
    }

    fn pop_frame(&mut self) {
        if let Some(frame) = self.stack.pop() {
            self.ignores
                .truncate(self.ignores.len().saturating_sub(frame.ignore_levels));
            self.identities.remove(&frame.identity);
        }
    }

    fn visit(&mut self, name: OsString) -> Option<Event> {
        let frame = self.stack.last().expect("visiting a listed frame");
        let depth = frame.depth + 1;
        let Some(utf8) = os::utf8_name(&name) else {
            return Some(Event::Skip {
                path: None,
                reason: Skip::NonUtf8Name,
                selected: true,
            });
        };
        if VCS_DIRECTORIES.contains(&utf8) {
            return None;
        }
        let mut canonical = frame.canonical.clone();
        canonical.push(name.clone());
        if !self.filter.include_hidden && utf8.starts_with('.') {
            return None;
        }
        let path = frame
            .path
            .join(utf8)
            .expect("a UTF-8 name without '/' joins");
        let parent = frame.directory.clone();
        let parent_canonical = frame.canonical.clone();
        let stat = match parent.stat(&name) {
            Ok(stat) => stat,
            Err(error) => {
                let error = io_error(error, &path);
                return self.skip_entry(path, Skip::Unreadable(error));
            }
        };
        if self.confinement.is_state(&stat) {
            return None;
        }
        let mut entry = Entry {
            path,
            depth,
            parent,
            name,
            canonical,
            stat,
            via_link: false,
            selected: false,
        };
        if stat.kind == FileKind::Symlink
            && self.links == LinkMode::Follow
            && let Err(skip) = self.follow(&parent_canonical, &mut entry)
        {
            return self.skip_entry(entry.path, skip);
        }
        let is_dir = entry.stat.is_dir();
        if self.ignored(&entry.path, is_dir) || self.filter.exclude.matches(&entry.path, is_dir) {
            return None;
        }
        entry.selected = self.filter.selects(&entry.path, is_dir);
        if is_dir {
            if entry.via_link && self.identities.contains(&(entry.stat.dev, entry.stat.ino)) {
                return Some(Event::Skip {
                    reason: Skip::LinkOutsideRoot(ProtocolError::failed_precondition(
                        PreconditionReason::SymlinkLoop,
                        format!("{}: the link closes a directory cycle", entry.path),
                    )),
                    path: Some(entry.path),
                    selected: true,
                });
            }
            match entry.parent.child(&entry.name) {
                Ok(directory) => {
                    let identity = (entry.stat.dev, entry.stat.ino);
                    self.identities.insert(identity);
                    self.stack.push(Frame {
                        directory: Arc::new(directory),
                        canonical: entry.canonical.clone(),
                        path: entry.path.clone(),
                        depth,
                        identity,
                        names: None,
                        ignore_levels: 0,
                    });
                }
                Err(error) => {
                    return Some(Event::Skip {
                        reason: Skip::Unreadable(io_error(error, &entry.path)),
                        path: Some(entry.path),
                        selected: true,
                    });
                }
            }
        }
        Some(Event::Entry(entry))
    }

    /// A skip of an entry whose kind is unknown (it vanished, or a link
    /// that could not be followed), taken as a non-directory: pruned like
    /// one when ignored or excluded, selected like one otherwise.
    fn skip_entry(&self, path: TreePath, reason: Skip) -> Option<Event> {
        if self.ignored(&path, false) || self.filter.exclude.matches(&path, false) {
            return None;
        }
        Some(Event::Skip {
            selected: self.filter.selects(&path, false),
            path: Some(path),
            reason,
        })
    }

    /// Replace a link entry by its in-root target.
    fn follow(&self, parent: &[OsString], entry: &mut Entry) -> Result<(), Skip> {
        let resolved = self
            .confinement
            .lookup_link(parent, &entry.name, &entry.path)
            .map_err(|error| match &error.data {
                environment_protocol::error::ErrorData::PermissionDenied {
                    reason: Some(PermissionReason::OutsideRoot | PermissionReason::DaemonState),
                }
                | environment_protocol::error::ErrorData::FailedPrecondition {
                    reason: PreconditionReason::SymlinkLoop,
                } => Skip::LinkOutsideRoot(error),
                environment_protocol::error::ErrorData::NotFound { .. } => Skip::DanglingLink,
                _ => Skip::Unreadable(error),
            })?;
        let stat = resolved.stat.expect("a followed lookup reports its target");
        entry.parent = Arc::new(resolved.parent);
        entry.name = resolved.name;
        entry.canonical = resolved.segments;
        entry.stat = stat;
        entry.via_link = true;
        Ok(())
    }

    fn ignored(&self, path: &TreePath, is_dir: bool) -> bool {
        if self.filter.include_ignored {
            return false;
        }
        let candidate = self.base.join(segments_path_tree(path));
        for level in self.ignores.iter().rev() {
            let matched = level.matcher.matched(&candidate, is_dir);
            if matched.is_ignore() {
                return true;
            }
            if matched.is_whitelist() {
                return false;
            }
        }
        false
    }

    fn load_ancestor_ignores(&mut self, root: &[OsString]) {
        for depth in 0..root.len() {
            let segments = &root[..depth];
            let Ok(resolved) = self.confinement.reopen(segments, &"ignore file ancestor") else {
                continue;
            };
            let Ok(directory) = resolved.open_directory() else {
                continue;
            };
            self.load_ignores(&directory, segments_path(segments));
        }
    }

    /// Push the ignore level of one directory; returns how many levels were
    /// pushed (0 or 1).
    fn load_ignores(&mut self, directory: &Directory, at: PathBuf) -> usize {
        let mut builder = GitignoreBuilder::new(&at);
        let mut lines = 0usize;
        let mut add = |content: Vec<u8>| {
            for line in String::from_utf8_lossy(&content).lines() {
                if builder.add_line(None, line).is_ok() {
                    lines += 1;
                }
            }
        };
        if let Ok(git) = directory.child(OsStr::new(".git"))
            && let Ok(info) = git.child(OsStr::new("info"))
            && let Some(content) = read_small(&info, "exclude")
        {
            add(content);
        }
        for name in [".gitignore", ".ignore"] {
            if let Some(content) = read_small(directory, name) {
                add(content);
            }
        }
        if lines == 0 {
            return 0;
        }
        match builder.build() {
            Ok(matcher) => {
                self.ignores.push(IgnoreLevel { matcher });
                1
            }
            Err(_) => 0,
        }
    }
}

fn read_small(directory: &Directory, name: &str) -> Option<Vec<u8>> {
    let mut file = directory.open_read(OsStr::new(name)).ok()?;
    if !EntryStat::of_file(&file).ok()?.is_file() {
        return None;
    }
    os::read_up_to(&mut file, MAX_IGNORE_FILE_BYTES).ok()
}

/// `/` joined with segments: the coordinate space of ignore matching.
fn segments_path(segments: &[OsString]) -> PathBuf {
    let mut path = PathBuf::from("/");
    path.extend(segments);
    path
}

fn segments_path_tree(path: &TreePath) -> PathBuf {
    path.segments().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::confinement::Lookup;
    use environment_protocol::paths::EnvironmentPath;

    fn set(patterns: &[&str]) -> PatternSet {
        PatternSet::compile(
            &patterns.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>(),
            false,
            "filter.include",
        )
        .unwrap()
    }

    fn tree(path: &str) -> TreePath {
        TreePath::new(path).unwrap()
    }

    #[test]
    fn patterns_follow_gitignore_anchoring() {
        let star = set(&["src/*.rs"]);
        assert!(star.matches(&tree("src/lib.rs"), false));
        assert!(!star.matches(&tree("src/deep/nested.rs"), false));

        let name = set(&["*.md"]);
        assert!(name.matches(&tree("readme.md"), false));
        assert!(name.matches(&tree("docs/deep/guide.md"), false));

        let anchored = set(&["/build"]);
        assert!(anchored.matches(&tree("build"), true));
        assert!(!anchored.matches(&tree("sub/build"), true));

        let spanning = set(&["**/SKILL.md"]);
        assert!(spanning.matches(&tree("SKILL.md"), false));
        assert!(spanning.matches(&tree("a/b/SKILL.md"), false));

        let directories = set(&["target/"]);
        assert!(directories.matches(&tree("crates/target"), true));
        assert!(!directories.matches(&tree("crates/target"), false));

        let alternation = set(&["*.{rs,toml}"]);
        assert!(alternation.matches(&tree("Cargo.toml"), false));

        assert!(PatternSet::compile(&["".into()], false, "filter.include").is_err());
        assert!(PatternSet::compile(&["[".into()], false, "filter.include").is_err());
    }

    fn walk(root: &std::path::Path, filter: TreeFilter, links: LinkMode) -> Vec<String> {
        let confinement = Arc::new(Confinement::new(root.into(), root.join(".state")).unwrap());
        let resolved = confinement
            .lookup(
                &EnvironmentPath::new(root.to_str().unwrap()).unwrap(),
                Lookup::FOLLOW,
            )
            .unwrap();
        let mut walker = Walker::new(
            confinement,
            &resolved,
            CompiledFilter::compile(&filter, "filter").unwrap(),
            links,
            None,
            &"root",
        )
        .unwrap();
        let mut seen = Vec::new();
        while let Some(event) = walker.next_event() {
            match event {
                Event::Entry(entry) if entry.selected => seen.push(entry.path.to_string()),
                Event::Entry(_) | Event::Yield => {}
                Event::Skip { path, .. } => seen.push(format!(
                    "skip:{}",
                    path.map(|path| path.to_string()).unwrap_or_default()
                )),
            }
        }
        seen
    }

    #[test]
    fn walks_prune_vcs_state_hidden_and_ignored_entries_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for directory in [
            ".git/objects",
            "src",
            "node_modules/pkg",
            ".hidden",
            ".state",
        ] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(root.join(".git/objects/pack"), "pack").unwrap();
        std::fs::write(root.join(".gitignore"), "node_modules/\n*.log\n!keep.log\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "lib").unwrap();
        std::fs::write(root.join("src/debug.log"), "log").unwrap();
        std::fs::write(root.join("src/keep.log"), "log").unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), "js").unwrap();
        std::fs::write(root.join(".hidden/file"), "hidden").unwrap();
        std::fs::write(root.join(".state/daemon-key"), "key").unwrap();

        assert_eq!(
            walk(&root, TreeFilter::default(), LinkMode::Report),
            ["src", "src/keep.log", "src/lib.rs"]
        );
        let everything = TreeFilter {
            include_hidden: true,
            include_ignored: true,
            ..TreeFilter::default()
        };
        // VCS metadata and the state directory stay pruned.
        assert_eq!(
            walk(&root, everything, LinkMode::Report),
            [
                ".gitignore",
                ".hidden",
                ".hidden/file",
                "node_modules",
                "node_modules/pkg",
                "node_modules/pkg/index.js",
                "src",
                "src/debug.log",
                "src/keep.log",
                "src/lib.rs",
            ]
        );
    }

    #[test]
    fn followed_links_stay_inside_the_root_and_cycles_are_skipped() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let root = base.join("root");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        std::fs::write(root.join("real/file"), "x").unwrap();
        symlink("real", root.join("alias")).unwrap();
        symlink("..", root.join("real/up")).unwrap();
        symlink(base.join("outside"), root.join("escape")).unwrap();

        let filter = TreeFilter {
            include_hidden: true,
            ..TreeFilter::default()
        };
        assert_eq!(
            walk(&root, filter, LinkMode::Follow),
            [
                "alias",
                "alias/file",
                "skip:alias/up",
                "skip:escape",
                "real",
                "real/file",
                "skip:real/up",
            ]
        );
    }
}
