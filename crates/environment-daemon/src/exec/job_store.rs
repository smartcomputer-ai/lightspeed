//! Job records and output on disk, written by one thread of their own.
//!
//! Each job has a directory `<jobs>/<jobId>/` with `record.json`, its
//! summary (rewritten only on state transitions), and its output as
//! append-only framed logs: `output.head` holds the bytes below the head
//! cap, `output.tail` the bytes after it. The tail log is compacted to the
//! newest tail-cap bytes once it holds twice that, so a verbose job writes
//! O(output) bytes in total and never more than the cap plus slack stays on
//! disk. Writes are buffered and flushed on an interval and when the job
//! finishes; nothing here runs on the runtime thread.
//!
//! Output waits for the store thread in a per-job pending buffer, not in
//! the command queue: of the bytes past the head cap only the newest
//! tail-cap bytes are kept there, so a job that produces faster than the
//! disk takes it holds at most its retained output in memory, and the
//! bytes dropped are ones the tail compaction would drop anyway.

use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufWriter, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, mpsc},
    time::{Duration, Instant},
};

use environment_protocol::{data::exec::OutputStream, ids::JobId};
use tokio::sync::oneshot;

use super::log::OutputLog;

/// How often buffered output reaches the disk while a job runs.
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

const RECORD_FILE: &str = "record.json";
const HEAD_FILE: &str = "output.head";
const TAIL_FILE: &str = "output.tail";

/// Frame header: offset (u64), stream (u8), length (u32), little endian.
const FRAME_HEADER_BYTES: usize = 13;

enum Command {
    WriteRecord {
        job_id: JobId,
        record: Vec<u8>,
        done: Option<oneshot::Sender<()>>,
    },
    /// The job has pending output.
    Drain {
        job_id: JobId,
    },
    /// Flushes and closes the job's output, then writes its final record.
    Finish {
        job_id: JobId,
        record: Vec<u8>,
        done: oneshot::Sender<()>,
    },
    Remove {
        job_id: JobId,
    },
}

/// A frame of output: offset, stream, bytes.
type Frame = (u64, OutputStream, Vec<u8>);

/// Output of one job not yet taken by the store thread.
#[derive(Default)]
struct Pending {
    /// In offset order; a frame lies wholly below or wholly past the head
    /// cap.
    frames: Vec<Frame>,
    /// Bytes of `frames` past the head cap.
    tail_bytes: u64,
    /// A drain command for the job is queued.
    queued: bool,
}

type PendingOutput = Arc<Mutex<BTreeMap<JobId, Pending>>>;

/// A handle to the store thread. Cheap to clone.
#[derive(Clone)]
pub(crate) struct JobStore {
    root: PathBuf,
    head_cap: u64,
    tail_cap: u64,
    pending: PendingOutput,
    sender: mpsc::Sender<Command>,
}

impl JobStore {
    /// Starts the store thread for job directories under `root`. Output at
    /// offsets below `head_cap` goes to the head log, the rest to the tail
    /// log, which keeps at least the newest `tail_cap` bytes.
    pub fn start(root: PathBuf, head_cap: u64, tail_cap: u64) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let pending = PendingOutput::default();
        let writer = Writer {
            root: root.clone(),
            head_cap,
            tail_cap,
            pending: pending.clone(),
            open: BTreeMap::new(),
        };
        std::thread::Builder::new()
            .name("envd-job-store".to_owned())
            .spawn(move || writer.run(receiver))?;
        Ok(Self {
            root,
            head_cap,
            tail_cap,
            pending,
            sender,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Queues a record write; the receiver answers once it is on disk.
    pub fn write_record(&self, job_id: &JobId, record: Vec<u8>) -> oneshot::Receiver<()> {
        let (done, written) = oneshot::channel();
        self.send(Command::WriteRecord {
            job_id: job_id.clone(),
            record,
            done: Some(done),
        });
        written
    }

    /// Queues a record write without waiting.
    pub fn queue_record(&self, job_id: &JobId, record: Vec<u8>) {
        self.send(Command::WriteRecord {
            job_id: job_id.clone(),
            record,
            done: None,
        });
    }

    /// Adds output at `offset` to the job's pending buffer, keeping its
    /// head bytes and its newest tail-cap bytes past the head cap.
    pub fn append(&self, job_id: &JobId, offset: u64, stream: OutputStream, data: &[u8]) {
        let head_len = self.head_cap.saturating_sub(offset).min(data.len() as u64) as usize;
        let (head, tail) = data.split_at(head_len);
        let wake = {
            let mut pending = lock(&self.pending);
            let entry = pending.entry(job_id.clone()).or_default();
            entry.push(offset, stream, head, self.head_cap);
            entry.push(offset + head_len as u64, stream, tail, self.head_cap);
            entry.tail_bytes += tail.len() as u64;
            entry.trim_tail(self.head_cap, self.tail_cap);
            !std::mem::replace(&mut entry.queued, true)
        };
        if wake {
            self.send(Command::Drain {
                job_id: job_id.clone(),
            });
        }
    }

    /// Flushes the job's output and writes its final record; answers once
    /// both are on disk.
    pub async fn finish(&self, job_id: &JobId, record: Vec<u8>) {
        let (done, written) = oneshot::channel();
        self.send(Command::Finish {
            job_id: job_id.clone(),
            record,
            done,
        });
        let _ = written.await;
    }

    pub fn remove(&self, job_id: &JobId) {
        self.send(Command::Remove {
            job_id: job_id.clone(),
        });
    }

    fn send(&self, command: Command) {
        if self.sender.send(command).is_err() {
            eprintln!("lightspeed-envd: the job store thread has stopped");
        }
    }
}

/// Reads a job's retained output back. `end` is the end recorded with the
/// job's final status; a larger end found in the logs wins (a job that was
/// interrupted before its final record).
pub(crate) fn load_output(dir: &Path, end: u64, cap: u64) -> std::io::Result<OutputLog> {
    let mut records = read_frames(&dir.join(HEAD_FILE))?;
    records.extend(read_frames(&dir.join(TAIL_FILE))?);
    records.sort_by_key(|(offset, _, _)| *offset);
    let mut log = OutputLog::from_records(records, end);
    log.enforce_cap(cap, 0);
    Ok(log)
}

struct OpenOutput {
    head: Option<BufWriter<File>>,
    tail: Option<BufWriter<File>>,
    tail_bytes: u64,
}

impl Pending {
    fn push(&mut self, offset: u64, stream: OutputStream, data: &[u8], head_cap: u64) {
        if data.is_empty() {
            return;
        }
        // Contiguous output of one stream on one side of the head cap
        // becomes one frame.
        if let Some((last_offset, last_stream, last_data)) = self.frames.last_mut()
            && *last_stream == stream
            && *last_offset + last_data.len() as u64 == offset
            && (*last_offset < head_cap) == (offset < head_cap)
        {
            last_data.extend_from_slice(data);
            return;
        }
        self.frames.push((offset, stream, data.to_vec()));
    }

    /// Drops the oldest bytes past the head cap beyond the newest
    /// `tail_cap`.
    fn trim_tail(&mut self, head_cap: u64, tail_cap: u64) {
        while self.tail_bytes > tail_cap {
            let Some(first) = self
                .frames
                .iter()
                .position(|(offset, _, _)| *offset >= head_cap)
            else {
                return;
            };
            let excess = self.tail_bytes - tail_cap;
            let (offset, _, data) = &mut self.frames[first];
            if data.len() as u64 <= excess {
                self.tail_bytes -= data.len() as u64;
                self.frames.remove(first);
            } else {
                data.drain(..excess as usize);
                *offset += excess;
                self.tail_bytes -= excess;
            }
        }
    }
}

struct Writer {
    root: PathBuf,
    head_cap: u64,
    tail_cap: u64,
    pending: PendingOutput,
    open: BTreeMap<JobId, OpenOutput>,
}

impl Writer {
    fn run(mut self, receiver: mpsc::Receiver<Command>) {
        let mut flushed = Instant::now();
        loop {
            // Flushes on the interval even while commands keep arriving.
            let wait = FLUSH_INTERVAL.saturating_sub(flushed.elapsed());
            match receiver.recv_timeout(wait) {
                Ok(command) => self.handle(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let jobs: Vec<JobId> = lock(&self.pending).keys().cloned().collect();
                    for job_id in jobs {
                        self.drain(&job_id);
                    }
                    self.flush_all();
                    return;
                }
            }
            if flushed.elapsed() >= FLUSH_INTERVAL {
                self.flush_all();
                flushed = Instant::now();
            }
        }
    }

    /// Writes the job's pending output.
    fn drain(&mut self, job_id: &JobId) {
        let frames = {
            let mut pending = lock(&self.pending);
            let Some(entry) = pending.get_mut(job_id) else {
                return;
            };
            entry.queued = false;
            entry.tail_bytes = 0;
            std::mem::take(&mut entry.frames)
        };
        for (offset, stream, data) in frames {
            if let Err(error) = self.append(job_id, offset, stream, &data) {
                eprintln!("lightspeed-envd: could not store output of job {job_id}: {error}");
                return;
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::WriteRecord {
                job_id,
                record,
                done,
            } => {
                self.write_record(&job_id, &record);
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
            Command::Drain { job_id } => self.drain(&job_id),
            Command::Finish {
                job_id,
                record,
                done,
            } => {
                self.drain(&job_id);
                lock(&self.pending).remove(&job_id);
                if let Some(mut output) = self.open.remove(&job_id) {
                    for writer in [output.head.as_mut(), output.tail.as_mut()]
                        .into_iter()
                        .flatten()
                    {
                        if let Err(error) = writer.flush() {
                            eprintln!(
                                "lightspeed-envd: could not flush output of job {job_id}: {error}"
                            );
                        }
                    }
                }
                self.write_record(&job_id, &record);
                let _ = done.send(());
            }
            Command::Remove { job_id } => {
                lock(&self.pending).remove(&job_id);
                self.open.remove(&job_id);
                let dir = self.root.join(job_id.as_str());
                if let Err(error) = std::fs::remove_dir_all(&dir)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    eprintln!("lightspeed-envd: could not remove job {job_id}: {error}");
                }
            }
        }
    }

    fn write_record(&self, job_id: &JobId, record: &[u8]) {
        let dir = self.root.join(job_id.as_str());
        let result = std::fs::create_dir_all(&dir).and_then(|()| {
            let path = dir.join(RECORD_FILE);
            let temp = dir.join(format!("{RECORD_FILE}.tmp"));
            std::fs::write(&temp, record)?;
            std::fs::rename(&temp, &path)
        });
        if let Err(error) = result {
            eprintln!("lightspeed-envd: could not persist job {job_id}: {error}");
        }
    }

    fn append(
        &mut self,
        job_id: &JobId,
        offset: u64,
        stream: OutputStream,
        data: &[u8],
    ) -> std::io::Result<()> {
        let head_cap = self.head_cap;
        let head_len = head_cap.saturating_sub(offset).min(data.len() as u64) as usize;
        let (head, tail) = data.split_at(head_len);
        let dir = self.root.join(job_id.as_str());
        if !self.open.contains_key(job_id) {
            std::fs::create_dir_all(&dir)?;
            self.open.insert(
                job_id.clone(),
                OpenOutput {
                    head: None,
                    tail: None,
                    tail_bytes: file_len(&dir.join(TAIL_FILE)),
                },
            );
        }
        let output = self.open.get_mut(job_id).expect("opened above");
        if !head.is_empty() {
            if output.head.is_none() {
                output.head = Some(BufWriter::new(append_to(&dir.join(HEAD_FILE))?));
            }
            let writer = output.head.as_mut().expect("opened above");
            write_frame(writer, offset, stream, head)?;
        }
        if !tail.is_empty() {
            if output.tail.is_none() {
                output.tail = Some(BufWriter::new(append_to(&dir.join(TAIL_FILE))?));
            }
            let writer = output.tail.as_mut().expect("opened above");
            write_frame(writer, offset + head_len as u64, stream, tail)?;
            output.tail_bytes += (FRAME_HEADER_BYTES + tail.len()) as u64;
            if output.tail_bytes > 2 * self.tail_cap + 64 * 1024 {
                let tail_cap = self.tail_cap;
                compact_tail(&dir, output, tail_cap)?;
            }
        }
        Ok(())
    }

    fn flush_all(&mut self) {
        for (job_id, output) in &mut self.open {
            for writer in [output.head.as_mut(), output.tail.as_mut()]
                .into_iter()
                .flatten()
            {
                if let Err(error) = writer.flush() {
                    eprintln!("lightspeed-envd: could not flush output of job {job_id}: {error}");
                }
            }
        }
    }
}

/// Rewrites the tail log with only its newest `tail_cap` bytes.
fn compact_tail(dir: &Path, output: &mut OpenOutput, tail_cap: u64) -> std::io::Result<()> {
    if let Some(mut writer) = output.tail.take() {
        writer.flush()?;
    }
    let path = dir.join(TAIL_FILE);
    let mut records = read_frames(&path)?;
    let mut kept = 0u64;
    let mut first = records.len();
    while first > 0 && kept < tail_cap {
        first -= 1;
        kept += records[first].2.len() as u64;
    }
    let mut records = records.split_off(first);
    if let Some((offset, _, data)) = records.first_mut()
        && kept > tail_cap
    {
        let excess = (kept - tail_cap) as usize;
        data.drain(..excess);
        *offset += excess as u64;
    }
    let temp = dir.join(format!("{TAIL_FILE}.tmp"));
    {
        let mut writer = BufWriter::new(File::create(&temp)?);
        for (offset, stream, data) in &records {
            write_frame(&mut writer, *offset, *stream, data)?;
        }
        writer.flush()?;
    }
    std::fs::rename(&temp, &path)?;
    output.tail_bytes = file_len(&path);
    output.tail = Some(BufWriter::new(append_to(&path)?));
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn append_to(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn write_frame(
    writer: &mut impl std::io::Write,
    offset: u64,
    stream: OutputStream,
    data: &[u8],
) -> std::io::Result<()> {
    let stream = match stream {
        OutputStream::Stdout => 0u8,
        OutputStream::Stderr => 1,
        OutputStream::Pty => 2,
    };
    let len = u32::try_from(data.len()).map_err(std::io::Error::other)?;
    writer.write_all(&offset.to_le_bytes())?;
    writer.write_all(&[stream])?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(data)
}

/// Every whole frame of a log; a frame cut short by a crash ends it.
fn read_frames(path: &Path) -> std::io::Result<Vec<(u64, OutputStream, Vec<u8>)>> {
    let mut bytes = Vec::new();
    match File::open(path) {
        Ok(mut file) => {
            file.read_to_end(&mut bytes)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    }
    let mut frames = Vec::new();
    let mut rest = bytes.as_slice();
    while rest.len() >= FRAME_HEADER_BYTES {
        let offset = u64::from_le_bytes(rest[..8].try_into().expect("8 bytes"));
        let stream = match rest[8] {
            0 => OutputStream::Stdout,
            1 => OutputStream::Stderr,
            2 => OutputStream::Pty,
            _ => break,
        };
        let len = u32::from_le_bytes(rest[9..13].try_into().expect("4 bytes")) as usize;
        let Some(data) = rest.get(FRAME_HEADER_BYTES..FRAME_HEADER_BYTES + len) else {
            break;
        };
        frames.push((offset, stream, data.to_vec()));
        rest = &rest[FRAME_HEADER_BYTES + len..];
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use environment_protocol::data::exec::GapReason;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn output_round_trips_with_head_tail_and_bounded_disk_use() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = JobStore::start(temp.path().to_owned(), 1000, 1000).expect("store");
        let job_id = JobId::new("verbose").unwrap();
        // 200 KiB through 8 KiB appends against a 1000-byte tail.
        let mut offset = 0u64;
        for i in 0..25u8 {
            let data = vec![i; 8192];
            store.append(&job_id, offset, OutputStream::Stdout, &data);
            offset += data.len() as u64;
        }
        store.finish(&job_id, b"{}".to_vec()).await;
        let dir = temp.path().join("verbose");
        assert!(
            file_len(&dir.join(TAIL_FILE))
                <= 2 * 1000 + 64 * 1024 + 8192 + 2 * FRAME_HEADER_BYTES as u64,
            "the tail log is compacted"
        );
        assert_eq!(
            file_len(&dir.join(HEAD_FILE)),
            1000 + FRAME_HEADER_BYTES as u64
        );
        assert_eq!(std::fs::read(dir.join(RECORD_FILE)).unwrap(), b"{}");

        let log = load_output(&dir, offset, 2000).expect("load");
        assert_eq!(log.end(), offset);
        let page = log.page(0, 10_000, 4096);
        assert_eq!(page.gaps.len(), 1);
        assert_eq!(page.gaps[0].reason, GapReason::Overflow);
        assert_eq!(page.gaps[0].offset, 1000);
        assert_eq!(page.gaps[0].omitted_bytes, offset - 2000);
        assert_eq!(page.bytes, 2000);
        assert_eq!(
            page.chunks.last().unwrap().data.as_slice().last(),
            Some(&24)
        );

        store.remove(&job_id);
        let _ = store
            .write_record(&JobId::new("other").unwrap(), b"{}".to_vec())
            .await;
        assert!(!dir.exists(), "commands run in order");
    }

    #[test]
    fn pending_output_keeps_the_head_and_the_newest_tail_only() {
        let mut pending = Pending::default();
        let (head_cap, tail_cap) = (100u64, 300u64);
        let mut offset = 0u64;
        for i in 0..50u8 {
            let data = [i; 64];
            let head_len = head_cap.saturating_sub(offset).min(64) as usize;
            let (head, tail) = data.split_at(head_len);
            pending.push(offset, OutputStream::Stdout, head, head_cap);
            pending.push(
                offset + head_len as u64,
                OutputStream::Stdout,
                tail,
                head_cap,
            );
            pending.tail_bytes += tail.len() as u64;
            pending.trim_tail(head_cap, tail_cap);
            offset += 64;
            let held: u64 = pending
                .frames
                .iter()
                .map(|(_, _, data)| data.len() as u64)
                .sum();
            assert!(held <= head_cap + tail_cap, "{held} bytes pending");
        }
        let frames = &pending.frames;
        assert_eq!(frames.len(), 2, "contiguous output coalesces per side");
        assert_eq!((frames[0].0, frames[0].2.len()), (0, 100));
        assert_eq!(frames[1].0, offset - tail_cap);
        assert_eq!(frames[1].2.len() as u64, tail_cap);
        assert_eq!(frames[1].2.last(), Some(&49));
    }

    #[test]
    fn a_frame_cut_short_ends_the_log() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("log");
        let mut bytes = Vec::new();
        write_frame(&mut bytes, 0, OutputStream::Stderr, b"whole").unwrap();
        write_frame(&mut bytes, 5, OutputStream::Stdout, b"cut").unwrap();
        bytes.truncate(bytes.len() - 1);
        std::fs::write(&path, bytes).unwrap();
        let frames = read_frames(&path).unwrap();
        assert_eq!(frames, vec![(0, OutputStream::Stderr, b"whole".to_vec())]);
    }
}
