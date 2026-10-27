//! `fs/searchText` and `fs/globFiles`: bounded walks run on a blocking
//! thread that stop at their duration ceiling, on `$/cancelRequest` and
//! when their connection closes.

use std::{
    io::{self, Read},
    sync::Arc,
    time::{Duration, Instant},
};

use environment_protocol::{
    data::{
        fs::{
            FileKind, GlobFilesParams, GlobFilesResponse, GlobFilesStop, SearchTextMatch,
            SearchTextParams, SearchTextResponse, SearchTextStop,
        },
        handshake::{GlobCeilings, SearchCeilings},
    },
    error::{PreconditionReason, ProtocolError},
    limits::MAX_RESULT_BYTES,
    paths::EnvironmentPath,
};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use tokio_util::sync::CancellationToken;

use super::{
    confinement::{Confinement, Lookup},
    json_string_len,
    os::EntryStat,
    walk::{CompiledFilter, Event, LinkMode, Walker},
};

/// Encoded overhead of one search match besides its encoded path and line.
const SEARCH_MATCH_OVERHEAD: u64 = 64;
/// Encoded overhead of one glob match besides its encoded path.
const GLOB_MATCH_OVERHEAD: u64 = 16;
/// Longest line the searcher buffers; a file with a longer line is skipped.
const MAX_SEARCH_LINE_BUFFER: usize = 16 * 1024 * 1024;

pub(crate) fn check_search(
    params: &SearchTextParams,
    ceilings: &SearchCeilings,
) -> Result<(), ProtocolError> {
    if params.pattern.is_empty() {
        return Err(ProtocolError::invalid_field(
            "pattern",
            "search pattern must not be empty",
        ));
    }
    let limits = &params.limits;
    check_quota(
        "limits.maxMatches",
        limits.max_matches.into(),
        ceilings.max_matches.into(),
    )?;
    check_quota(
        "limits.maxFiles",
        limits.max_files.into(),
        ceilings.max_files.into(),
    )?;
    check_quota("limits.maxBytes", limits.max_bytes, ceilings.max_bytes)?;
    check_quota(
        "limits.maxDurationMs",
        limits.max_duration_ms,
        ceilings.max_duration_ms,
    )?;
    check_quota(
        "limits.maxLineBytes",
        limits.max_line_bytes.into(),
        ceilings.max_line_bytes.into(),
    )
}

pub(crate) fn check_glob(
    params: &GlobFilesParams,
    ceilings: &GlobCeilings,
) -> Result<(), ProtocolError> {
    if params.filter.include.is_empty() {
        return Err(ProtocolError::invalid_field(
            "filter.include",
            "globFiles needs at least one include pattern",
        ));
    }
    let limits = &params.limits;
    check_quota(
        "limits.maxMatches",
        limits.max_matches.into(),
        ceilings.max_matches.into(),
    )?;
    check_quota(
        "limits.maxFiles",
        limits.max_files.into(),
        ceilings.max_files.into(),
    )?;
    check_quota(
        "limits.maxDurationMs",
        limits.max_duration_ms,
        ceilings.max_duration_ms,
    )
}

/// A caller quota: at least 1 and at most its ceiling.
pub(crate) fn check_quota(field: &str, value: u64, ceiling: u64) -> Result<(), ProtocolError> {
    if value == 0 {
        return Err(ProtocolError::invalid_field(
            field,
            format!("{field} must be at least 1"),
        ));
    }
    if value > ceiling {
        return Err(ProtocolError::above_ceiling(field, value, ceiling));
    }
    Ok(())
}

/// Resolve a walk root: an existing directory under the confinement rule.
fn walker(
    confinement: &Arc<Confinement>,
    root: &EnvironmentPath,
    filter: CompiledFilter,
    links: LinkMode,
) -> Result<Walker, ProtocolError> {
    let resolved = confinement.lookup(root, Lookup::FOLLOW)?;
    if !resolved.stat.is_some_and(|stat| stat.is_dir()) {
        return Err(ProtocolError::failed_precondition(
            PreconditionReason::NotDirectory,
            format!("{root} is not a directory"),
        ));
    }
    Walker::new(confinement.clone(), &resolved, filter, links, None, root)
}

pub(crate) fn search_text(
    confinement: &Arc<Confinement>,
    params: SearchTextParams,
    cancel: &CancellationToken,
) -> Result<SearchTextResponse, ProtocolError> {
    let started = Instant::now();
    let limits = params.limits;
    let matcher = grep_regex::RegexMatcherBuilder::new()
        .case_insensitive(params.pattern_ignore_case)
        .build(&params.pattern)
        .map_err(|error| {
            ProtocolError::invalid_field("pattern", format!("invalid search regex: {error}"))
        })?;
    let filter = CompiledFilter::compile(&params.filter, "filter")?;
    let mut walker = walker(confinement, &params.root, filter, LinkMode::Report)?;
    let deadline = Duration::from_millis(limits.max_duration_ms);
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0))
        .heap_limit(Some(MAX_SEARCH_LINE_BUFFER))
        .build();
    let mut response = SearchTextResponse {
        matches: Vec::new(),
        searched_file_count: 0,
        searched_bytes: 0,
        skipped_file_count: 0,
        skipped_entry_count: 0,
        elapsed_ms: 0,
        stopped: None,
    };
    let mut result_bytes = 0u64;
    while let Some(event) = walker.next_event() {
        if cancel.is_cancelled() {
            return Err(ProtocolError::cancelled());
        }
        if started.elapsed() >= deadline {
            response.stopped = Some(SearchTextStop::TimeLimit);
            break;
        }
        let entry = match event {
            Event::Yield => continue,
            Event::Skip { .. } => {
                response.skipped_entry_count = response.skipped_entry_count.saturating_add(1);
                continue;
            }
            Event::Entry(entry) => entry,
        };
        if entry.stat.kind != FileKind::File || !entry.selected {
            continue;
        }
        let remaining = limits.max_bytes - response.searched_bytes;
        if remaining == 0 {
            response.stopped = Some(SearchTextStop::ByteLimit);
            break;
        }
        if response.searched_file_count >= limits.max_files {
            response.stopped = Some(SearchTextStop::FileLimit);
            break;
        }
        if entry.stat.size > remaining {
            response.skipped_file_count = response.skipped_file_count.saturating_add(1);
            continue;
        }
        let Ok(file) = entry.parent.open_read(&entry.name) else {
            response.skipped_file_count = response.skipped_file_count.saturating_add(1);
            continue;
        };
        if !EntryStat::of_file(&file).is_ok_and(|stat| stat.is_file()) {
            response.skipped_file_count = response.skipped_file_count.saturating_add(1);
            continue;
        }
        let path = params
            .root
            .join(&entry.path)
            .map_err(|error| ProtocolError::internal(error.to_string()))?;
        let mut sink = MatchSink {
            path,
            matches: &mut response.matches,
            max_matches: limits.max_matches,
            max_line_bytes: limits.max_line_bytes as usize,
            result_bytes: &mut result_bytes,
            stopped: None,
            binary: false,
        };
        let mut reader = Counting {
            inner: file.take(remaining),
            read: 0,
            deadline: started + deadline,
            cancel,
            interrupted: None,
        };
        let outcome = searcher.search_reader(&matcher, &mut reader, &mut sink);
        let (stopped, binary) = (sink.stopped, sink.binary);
        response.searched_bytes += reader.read;
        match reader.interrupted {
            Some(Interrupted::Cancelled) => return Err(ProtocolError::cancelled()),
            Some(Interrupted::Deadline) => {
                response.stopped = Some(SearchTextStop::TimeLimit);
                break;
            }
            None => {}
        }
        if outcome.is_err() || binary {
            response.skipped_file_count = response.skipped_file_count.saturating_add(1);
        } else {
            response.searched_file_count += 1;
        }
        if stopped.is_some() {
            response.stopped = stopped;
            break;
        }
    }
    response.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(response)
}

/// Why a file's search stopped before its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interrupted {
    Cancelled,
    Deadline,
}

/// Counts the bytes read and ends the read once the walk's deadline passed
/// or its request was cancelled, so one large file cannot outlast either.
struct Counting<'a, R> {
    inner: R,
    read: u64,
    deadline: Instant,
    cancel: &'a CancellationToken,
    interrupted: Option<Interrupted>,
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            self.interrupted = Some(Interrupted::Cancelled);
        } else if Instant::now() >= self.deadline {
            self.interrupted = Some(Interrupted::Deadline);
        }
        if self.interrupted.is_some() {
            // Not `Interrupted`: readers retry that kind.
            return Err(io::Error::other("the search was stopped"));
        }
        let read = self.inner.read(buffer)?;
        self.read += read as u64;
        Ok(read)
    }
}

struct MatchSink<'a> {
    path: EnvironmentPath,
    matches: &'a mut Vec<SearchTextMatch>,
    max_matches: u32,
    max_line_bytes: usize,
    result_bytes: &'a mut u64,
    stopped: Option<SearchTextStop>,
    binary: bool,
}

impl Sink for MatchSink<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, found: &SinkMatch<'_>) -> Result<bool, io::Error> {
        if self.matches.len() as u64 >= u64::from(self.max_matches) {
            self.stopped = Some(SearchTextStop::MatchLimit);
            return Ok(false);
        }
        let line = String::from_utf8_lossy(found.bytes());
        let line = line.trim_end_matches(['\r', '\n']);
        let (line, line_truncated) = cut(line, self.max_line_bytes);
        let size =
            json_string_len(self.path.as_str()) + json_string_len(line) + SEARCH_MATCH_OVERHEAD;
        if *self.result_bytes + size > MAX_RESULT_BYTES {
            self.stopped = Some(SearchTextStop::ResponseLimit);
            return Ok(false);
        }
        *self.result_bytes += size;
        let line_number = found
            .line_number()
            .map_or(0, |number| u32::try_from(number).unwrap_or(u32::MAX));
        self.matches.push(SearchTextMatch {
            path: self.path.clone(),
            line_number,
            line: line.to_owned(),
            line_truncated,
        });
        Ok(true)
    }

    fn binary_data(&mut self, _: &Searcher, _: u64) -> Result<bool, io::Error> {
        self.binary = true;
        Ok(false)
    }
}

/// Cut `line` to at most `max` bytes on a character boundary.
fn cut(line: &str, max: usize) -> (&str, bool) {
    if line.len() <= max {
        return (line, false);
    }
    let mut end = max;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    (&line[..end], true)
}

pub(crate) fn glob_files(
    confinement: &Arc<Confinement>,
    params: GlobFilesParams,
    cancel: &CancellationToken,
) -> Result<GlobFilesResponse, ProtocolError> {
    let started = Instant::now();
    let limits = params.limits;
    let filter = CompiledFilter::compile(&params.filter, "filter")?;
    let mut walker = walker(confinement, &params.root, filter, LinkMode::Report)?;
    let deadline = Duration::from_millis(limits.max_duration_ms);
    let mut response = GlobFilesResponse {
        matches: Vec::new(),
        visited_file_count: 0,
        skipped_entry_count: 0,
        elapsed_ms: 0,
        stopped: None,
    };
    let mut result_bytes = 0u64;
    while let Some(event) = walker.next_event() {
        if cancel.is_cancelled() {
            return Err(ProtocolError::cancelled());
        }
        if started.elapsed() >= deadline {
            response.stopped = Some(GlobFilesStop::TimeLimit);
            break;
        }
        let entry = match event {
            Event::Yield => continue,
            Event::Skip { .. } => {
                response.skipped_entry_count = response.skipped_entry_count.saturating_add(1);
                continue;
            }
            Event::Entry(entry) => entry,
        };
        if entry.stat.is_dir() {
            continue;
        }
        if response.visited_file_count >= limits.max_files {
            response.stopped = Some(GlobFilesStop::FileLimit);
            break;
        }
        response.visited_file_count += 1;
        if !entry.selected {
            continue;
        }
        if response.matches.len() as u64 >= u64::from(limits.max_matches) {
            response.stopped = Some(GlobFilesStop::MatchLimit);
            break;
        }
        let path = params
            .root
            .join(&entry.path)
            .map_err(|error| ProtocolError::internal(error.to_string()))?;
        let size = json_string_len(path.as_str()) + GLOB_MATCH_OVERHEAD;
        if result_bytes + size > MAX_RESULT_BYTES {
            response.stopped = Some(GlobFilesStop::ResponseLimit);
            break;
        }
        result_bytes += size;
        response.matches.push(path);
    }
    response.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment_protocol::data::fs::{GlobFilesLimits, SearchTextLimits, TreeFilter};

    struct Fixture {
        _temp: tempfile::TempDir,
        root: EnvironmentPath,
        host: std::path::PathBuf,
        confinement: Arc<Confinement>,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(host.join("src/deep")).unwrap();
        std::fs::create_dir_all(host.join(".git/objects")).unwrap();
        std::fs::write(
            host.join("src/lib.rs"),
            "pub fn target() {}\nfn other() {}\n",
        )
        .unwrap();
        std::fs::write(host.join("src/deep/nested.rs"), "target nested\n").unwrap();
        std::fs::write(host.join("readme.md"), "target readme\n").unwrap();
        std::fs::write(host.join("binary.bin"), b"tar\x00get\n").unwrap();
        // A pack file larger than any test budget; VCS metadata is pruned
        // before the budget is charged.
        std::fs::write(host.join(".git/objects/pack"), vec![b't'; 64 * 1024]).unwrap();
        let confinement = Arc::new(Confinement::new(host.clone(), host.join(".state")).unwrap());
        Fixture {
            _temp: temp,
            root: EnvironmentPath::new(host.to_str().unwrap()).unwrap(),
            host,
            confinement,
        }
    }

    fn search(root: &EnvironmentPath) -> SearchTextParams {
        SearchTextParams {
            root: root.clone(),
            pattern: "target".into(),
            pattern_ignore_case: false,
            filter: TreeFilter::default(),
            limits: SearchTextLimits {
                max_matches: 100,
                max_files: 100,
                max_bytes: 1024,
                max_duration_ms: 10_000,
                max_line_bytes: 4096,
            },
        }
    }

    #[test]
    fn search_skips_vcs_metadata_and_oversized_files_without_stopping() {
        let fixture = fixture();
        std::fs::write(fixture.host.join("big.txt"), vec![b'x'; 2048]).unwrap();
        let response = search_text(
            &fixture.confinement,
            search(&fixture.root),
            &CancellationToken::new(),
        )
        .unwrap();
        let paths: Vec<_> = response
            .matches
            .iter()
            .map(|found| {
                found
                    .path
                    .as_str()
                    .trim_start_matches(fixture.root.as_str())
            })
            .collect();
        assert_eq!(paths, ["/readme.md", "/src/deep/nested.rs", "/src/lib.rs"]);
        assert_eq!(response.stopped, None);
        // big.txt exceeds the remaining budget and binary.bin is binary:
        // both are skipped and counted, and the walk continues.
        assert_eq!(response.skipped_file_count, 2);
        assert!(response.searched_bytes < 1024);
    }

    #[test]
    fn search_stops_on_limits_and_cancellation() {
        let fixture = fixture();
        let mut params = search(&fixture.root);
        params.limits.max_matches = 1;
        let response =
            search_text(&fixture.confinement, params, &CancellationToken::new()).unwrap();
        assert_eq!(response.matches.len(), 1);
        assert_eq!(response.stopped, Some(SearchTextStop::MatchLimit));

        let mut params = search(&fixture.root);
        params.limits.max_line_bytes = 3;
        let response =
            search_text(&fixture.confinement, params, &CancellationToken::new()).unwrap();
        assert!(response.matches.iter().all(|found| found.line_truncated));

        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = search_text(&fixture.confinement, search(&fixture.root), &cancel).unwrap_err();
        assert_eq!(error, ProtocolError::cancelled());
    }

    #[test]
    fn glob_uses_the_tree_grammar_and_counts_only_files() {
        let fixture = fixture();
        let params = |patterns: &[&str]| GlobFilesParams {
            root: fixture.root.clone(),
            filter: TreeFilter {
                include: patterns.iter().map(|p| (*p).to_owned()).collect(),
                ..TreeFilter::default()
            },
            limits: GlobFilesLimits {
                max_matches: 100,
                max_files: 100,
                max_duration_ms: 10_000,
            },
        };
        let token = CancellationToken::new();
        let response = glob_files(&fixture.confinement, params(&["src/*.rs"]), &token).unwrap();
        assert_eq!(response.matches.len(), 1);
        assert!(response.matches[0].as_str().ends_with("/src/lib.rs"));
        let response = glob_files(&fixture.confinement, params(&["*.rs"]), &token).unwrap();
        assert_eq!(response.matches.len(), 2);
        // Directories are free; the four visible files are counted.
        assert_eq!(response.visited_file_count, 4);
        let error = check_glob(
            &params(&[]),
            &GlobCeilings {
                max_matches: 10,
                max_files: 10,
                max_duration_ms: 10,
            },
        )
        .unwrap_err();
        assert!(matches!(
            error.data,
            environment_protocol::error::ErrorData::InvalidParams { .. }
        ));
    }
}
