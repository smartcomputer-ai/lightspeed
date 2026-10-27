//! A command's input: a bounded queue drained by its own writer.
//!
//! The writer runs off the runtime thread: a Tokio task for a
//! non-blocking pipe, a thread of its own for a PTY master, whose writes
//! block when the child stops reading. Callers only enqueue, so a
//! child that never reads can fill the queue but never stall the daemon,
//! and signals still reach it while a write is pending.

use std::{
    io::Write as _,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use environment_protocol::error::{PreconditionReason, ProtocolError};
use tokio::{io::AsyncWriteExt as _, process::ChildStdin, sync::mpsc};

/// `VEOF` in the default terminal settings (Ctrl-D): end of input for a
/// canonical-mode reader at the start of a line.
const PTY_END_OF_INPUT: u8 = 0x04;

/// Where queued input goes.
pub(crate) enum InputSink {
    Pipe(ChildStdin),
    Pty(Box<dyn std::io::Write + Send>),
}

enum Message {
    Data(Vec<u8>),
    Close,
}

#[derive(Default)]
struct Shared {
    queued: AtomicU64,
    /// The input takes no more bytes: closed by the caller, by the child
    /// (broken pipe), or because the process exited.
    closed: AtomicBool,
}

/// The input of one execution.
pub(crate) struct Input {
    /// `process/write` is served: a pipe or a PTY.
    writable: bool,
    capacity: u64,
    shared: Arc<Shared>,
    sender: Mutex<Option<mpsc::UnboundedSender<Message>>>,
    /// The sink and the queue's receiver until the writer starts.
    pending: Mutex<Option<(InputSink, mpsc::UnboundedReceiver<Message>)>>,
}

impl Input {
    /// No input at all: `/dev/null`.
    pub fn none() -> Self {
        let shared = Arc::new(Shared::default());
        shared.closed.store(true, Ordering::Release);
        Self {
            writable: false,
            capacity: 0,
            shared,
            sender: Mutex::new(None),
            pending: Mutex::new(None),
        }
    }

    /// Input to `sink`. `initial` is queued first whatever the capacity;
    /// `close_after_initial` then ends the input. `writable` inputs accept
    /// `process/write` up to `capacity` queued bytes.
    pub fn new(
        sink: InputSink,
        initial: Option<Vec<u8>>,
        close_after_initial: bool,
        writable: bool,
        capacity: u64,
    ) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared::default());
        if let Some(initial) = initial.filter(|initial| !initial.is_empty()) {
            shared
                .queued
                .fetch_add(initial.len() as u64, Ordering::AcqRel);
            let _ = sender.send(Message::Data(initial));
        }
        let sender = if close_after_initial {
            let _ = sender.send(Message::Close);
            shared.closed.store(true, Ordering::Release);
            None
        } else {
            Some(sender)
        };
        Self {
            writable,
            capacity,
            shared,
            sender: Mutex::new(sender),
            pending: Mutex::new(Some((sink, receiver))),
        }
    }

    /// Starts the writer. Called once the output readers, the exit watcher
    /// and the deadline are in place, so input can never block them.
    pub fn start(&self) {
        let Some((sink, receiver)) = self.pending.lock().expect("input poisoned").take() else {
            return;
        };
        let shared = self.shared.clone();
        match sink {
            InputSink::Pipe(stdin) => {
                tokio::spawn(write_pipe(stdin, receiver, shared));
            }
            InputSink::Pty(writer) => {
                // A thread of its own, not the blocking pool: a write the
                // child never drains must not hold up the daemon's shutdown.
                let closed = shared.clone();
                if std::thread::Builder::new()
                    .name("envd-pty-writer".to_owned())
                    .spawn(move || write_pty(writer, receiver, shared))
                    .is_err()
                {
                    closed.closed.store(true, Ordering::Release);
                }
            }
        }
    }

    /// Queues a prefix of `data` and answers how many bytes were accepted;
    /// `close` ends the input once every byte was accepted.
    pub fn offer(&self, data: &[u8], close: bool) -> Result<u64, ProtocolError> {
        let mut sender = self.sender.lock().expect("input poisoned");
        let open = sender
            .as_ref()
            .filter(|_| self.writable && !self.shared.closed.load(Ordering::Acquire));
        let Some(open) = open else {
            return Err(ProtocolError::failed_precondition(
                PreconditionReason::InputClosed,
                if self.writable {
                    "the process input is closed"
                } else {
                    "the process was started without writable input"
                },
            ));
        };
        let queued = self.shared.queued.load(Ordering::Acquire);
        let free = self.capacity.saturating_sub(queued);
        let accepted = (data.len() as u64).min(free);
        if accepted > 0 {
            self.shared.queued.fetch_add(accepted, Ordering::AcqRel);
            if open
                .send(Message::Data(data[..accepted as usize].to_vec()))
                .is_err()
            {
                self.shared.queued.fetch_sub(accepted, Ordering::AcqRel);
                self.shared.closed.store(true, Ordering::Release);
                return Err(ProtocolError::failed_precondition(
                    PreconditionReason::InputClosed,
                    "the process input is closed",
                ));
            }
        }
        if close && accepted == data.len() as u64 {
            let _ = open.send(Message::Close);
            self.shared.closed.store(true, Ordering::Release);
            *sender = None;
        }
        Ok(accepted)
    }

    /// Ends the input without a close message, because the process exited.
    /// A writer stuck on a full PTY stays on its blocking thread until the
    /// write fails; nothing waits for it.
    pub fn shut(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.sender.lock().expect("input poisoned").take();
        self.pending.lock().expect("input poisoned").take();
    }

    /// Bytes queued and not yet written.
    #[cfg(test)]
    pub fn queued_bytes(&self) -> u64 {
        self.shared.queued.load(Ordering::Acquire)
    }
}

async fn write_pipe(
    stdin: ChildStdin,
    mut receiver: mpsc::UnboundedReceiver<Message>,
    shared: Arc<Shared>,
) {
    let mut stdin = Some(stdin);
    while let Some(message) = receiver.recv().await {
        match message {
            Message::Data(data) => {
                if let Some(writer) = stdin.as_mut()
                    && writer.write_all(&data).await.is_err()
                {
                    // Broken pipe: the child closed its input, which is its
                    // right, not a failure of the command.
                    stdin = None;
                    shared.closed.store(true, Ordering::Release);
                }
                shared.queued.fetch_sub(data.len() as u64, Ordering::AcqRel);
            }
            Message::Close => {
                stdin = None;
                shared.closed.store(true, Ordering::Release);
            }
        }
    }
}

fn write_pty(
    writer: Box<dyn std::io::Write + Send>,
    mut receiver: mpsc::UnboundedReceiver<Message>,
    shared: Arc<Shared>,
) {
    let mut writer = Some(writer);
    while let Some(message) = receiver.blocking_recv() {
        let (data, close) = match message {
            Message::Data(data) => (data, false),
            Message::Close => (vec![PTY_END_OF_INPUT], true),
        };
        if let Some(pty) = writer.as_mut()
            && pty.write_all(&data).and_then(|()| pty.flush()).is_err()
        {
            writer = None;
            shared.closed.store(true, Ordering::Release);
        }
        if close {
            shared.closed.store(true, Ordering::Release);
        } else {
            shared.queued.fetch_sub(data.len() as u64, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use environment_protocol::error::{ErrorData, ProtocolErrorKind};

    use super::*;

    fn is_input_closed(error: &ProtocolError) -> bool {
        matches!(
            error.data,
            ErrorData::FailedPrecondition {
                reason: PreconditionReason::InputClosed
            }
        )
    }

    /// A writer that never makes progress, like a PTY whose child stopped
    /// reading.
    struct Stuck(std::sync::mpsc::Receiver<()>);

    impl std::io::Write for Stuck {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.recv();
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_stuck_writer_fills_the_queue_without_blocking_the_caller() {
        let (release, wait) = std::sync::mpsc::channel();
        let input = Input::new(
            InputSink::Pty(Box::new(Stuck(wait))),
            None,
            false,
            true,
            1024,
        );
        input.start();
        let started = std::time::Instant::now();
        assert_eq!(input.offer(&[b'x'; 4096], false).unwrap(), 1024);
        assert_eq!(input.offer(b"more", false).unwrap(), 0, "queue full");
        assert!(started.elapsed() < Duration::from_secs(1));
        // The runtime thread is free: a timer still fires.
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(release);
        for _ in 0..200 {
            if input.offer(b"x", false).is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let error = input.offer(b"x", false).unwrap_err();
        assert!(is_input_closed(&error), "a broken pipe closes the input");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_applies_only_when_every_byte_was_accepted() {
        let (_release, wait) = std::sync::mpsc::channel();
        let input = Input::new(InputSink::Pty(Box::new(Stuck(wait))), None, false, true, 4);
        assert_eq!(input.offer(b"abcdef", true).unwrap(), 4);
        assert_eq!(input.queued_bytes(), 4);
        assert_eq!(input.offer(b"", false).unwrap(), 0, "still open");
        let none = Input::none();
        let error = none.offer(b"x", false).unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::FailedPrecondition);
        assert!(is_input_closed(&error));
    }
}
