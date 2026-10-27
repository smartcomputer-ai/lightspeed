//! The execution primitive processes and jobs share.
//!
//! One [`Execution`] is one root process spawned from a
//! [`CommandSpec`]: it runs as the leader of its own process group, its
//! stdout, stderr or PTY output is redacted and captured into a byte-offset
//! [`OutputSink`], its input is fed by a writer of its own, and its exit is
//! observed independently of its pipes. `process/*` and `job/*` differ only
//! in what they do around it: processes keep a daemon cursor and leave
//! leftovers running; jobs persist their output and sweep their group.
//!
//! Start order: validate, spawn, attach the output readers, the exit
//! watcher and the deadline, and only then start the input writer, so no
//! amount of input can block the start or keep a timeout from firing.

mod execution;
mod input;
pub(crate) mod job_store;
pub(crate) mod log;
pub(crate) mod outcome;
pub(crate) mod spawn_record;

use std::{collections::BTreeMap, path::PathBuf, process::Stdio};

use environment_protocol::{
    bytes::SecretString,
    data::exec::{CommandSpec, ExecInput, PtySize},
    error::ProtocolError,
};
use portable_pty::{CommandBuilder, native_pty_system};
use tokio::process::Command;

use crate::{process_group, redaction::redactions_for_secret_env};

pub(crate) use execution::{AfterExit, ExecOptions, Execution, OutputSink};
use input::{Input, InputSink};

/// Checks a command before anything runs. `field` is the wire path of the
/// command, such as `command` or `jobs[2].command`.
pub(crate) fn validate_command(spec: &CommandSpec, field: &str) -> Result<(), ProtocolError> {
    if spec.argv.is_empty() {
        return Err(ProtocolError::invalid_field(
            format!("{field}.argv"),
            "argv must name a program",
        ));
    }
    if spec.argv[0].is_empty() {
        return Err(ProtocolError::invalid_field(
            format!("{field}.argv"),
            "argv[0] must not be empty",
        ));
    }
    if spec.argv.iter().any(|arg| arg.contains('\0')) {
        return Err(ProtocolError::invalid_field(
            format!("{field}.argv"),
            "arguments must not contain NUL",
        ));
    }
    for (name, value) in &spec.env {
        validate_env_name(name, &format!("{field}.env"))?;
        if value.contains('\0') {
            return Err(ProtocolError::invalid_field(
                format!("{field}.env"),
                format!("value of {name} contains NUL"),
            ));
        }
    }
    for (name, value) in &spec.secret_env {
        validate_env_name(name, &format!("{field}.secretEnv"))?;
        if value.expose().contains('\0') {
            return Err(ProtocolError::invalid_field(
                format!("{field}.secretEnv"),
                format!("value of {name} contains NUL"),
            ));
        }
        if spec.env.contains_key(name) {
            return Err(ProtocolError::invalid_field(
                format!("{field}.secretEnv"),
                format!("{name} is also set in env"),
            ));
        }
    }
    if spec.timeout_ms == Some(0) {
        return Err(ProtocolError::invalid_field(
            format!("{field}.timeoutMs"),
            "timeoutMs must be positive; omit it for no deadline",
        ));
    }
    Ok(())
}

fn validate_env_name(name: &str, field: &str) -> Result<(), ProtocolError> {
    if name.is_empty() || name.contains('=') || name.contains('\0') {
        return Err(ProtocolError::invalid_field(
            field,
            format!("invalid variable name {name:?}"),
        ));
    }
    Ok(())
}

/// A validated command with its working directory resolved, ready to
/// spawn.
pub(crate) struct PreparedCommand {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub secret_env: BTreeMap<String, SecretString>,
    /// Daemon variables no child inherits.
    pub scrubbed_env: Vec<String>,
    pub input: ExecInput,
    pub pty: Option<PtySize>,
}

impl PreparedCommand {
    pub fn new(
        spec: CommandSpec,
        cwd: PathBuf,
        scrubbed_env: &[String],
        pty: Option<PtySize>,
    ) -> Self {
        Self {
            argv: spec.argv,
            cwd,
            env: spec.env,
            secret_env: spec.secret_env,
            scrubbed_env: scrubbed_env.to_vec(),
            input: spec.input,
            pty,
        }
    }

    /// Spawns the root and starts capture, exit watching and the deadline.
    /// The input writer starts with [`Execution::start_input`].
    pub fn spawn<S: OutputSink>(
        self,
        sink: S,
        options: ExecOptions,
        input_queue_bytes: u64,
    ) -> Result<Execution<S>, ProtocolError> {
        let redactions = redactions_for_secret_env(&self.secret_env);
        if let Some(size) = self.pty {
            self.spawn_pty(size, sink, options, redactions, input_queue_bytes)
        } else {
            self.spawn_pipes(sink, options, redactions, input_queue_bytes)
        }
    }

    fn spawn_pipes<S: OutputSink>(
        self,
        sink: S,
        options: ExecOptions,
        redactions: Vec<Vec<u8>>,
        input_queue_bytes: u64,
    ) -> Result<Execution<S>, ProtocolError> {
        let mut command = Command::new(&self.argv[0]);
        command
            .args(&self.argv[1..])
            .current_dir(&self.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(match self.input {
                ExecInput::None {} => Stdio::null(),
                ExecInput::Bytes { .. } | ExecInput::Pipe {} => Stdio::piped(),
            });
        for name in &self.scrubbed_env {
            command.env_remove(name);
        }
        command.envs(&self.env);
        for (name, value) in &self.secret_env {
            command.env(name, value.expose());
        }
        process_group::spawn_in_own_group(&mut command);
        let mut child = command.spawn().map_err(|error| {
            ProtocolError::process_failed(format!("spawn {:?}: {error}", self.argv[0]))
        })?;
        let pid = child
            .id()
            .ok_or_else(|| ProtocolError::process_failed("spawned process has no pid"))?;
        let input = match (self.input, child.stdin.take()) {
            (ExecInput::Bytes { data }, Some(stdin)) => Input::new(
                InputSink::Pipe(stdin),
                Some(data.into_inner()),
                true,
                false,
                0,
            ),
            (ExecInput::Pipe {}, Some(stdin)) => {
                Input::new(InputSink::Pipe(stdin), None, false, true, input_queue_bytes)
            }
            _ => Input::none(),
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Ok(Execution::start_pipes(
            pid, child, stdout, stderr, input, sink, redactions, options,
        ))
    }

    fn spawn_pty<S: OutputSink>(
        self,
        size: PtySize,
        sink: S,
        options: ExecOptions,
        redactions: Vec<Vec<u8>>,
        input_queue_bytes: u64,
    ) -> Result<Execution<S>, ProtocolError> {
        let pair = native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: u16::try_from(size.rows).unwrap_or(u16::MAX),
                cols: u16::try_from(size.cols).unwrap_or(u16::MAX),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| ProtocolError::process_failed(format!("open PTY: {error}")))?;
        let mut command = CommandBuilder::new(&self.argv[0]);
        command.args(&self.argv[1..]);
        command.cwd(&self.cwd);
        for name in &self.scrubbed_env {
            command.env_remove(name);
        }
        for (name, value) in &self.env {
            command.env(name, value);
        }
        for (name, value) in &self.secret_env {
            command.env(name, value.expose());
        }
        // Every fallible step on the master comes before the spawn, so a
        // started child is never left unsupervised by a failed start.
        let master = pair.master;
        let reader = master
            .try_clone_reader()
            .map_err(|error| ProtocolError::process_failed(format!("PTY reader: {error}")))?;
        let writer = master
            .take_writer()
            .map_err(|error| ProtocolError::process_failed(format!("PTY writer: {error}")))?;
        let mut child = pair.slave.spawn_command(command).map_err(|error| {
            ProtocolError::process_failed(format!("spawn {:?} on a PTY: {error}", self.argv[0]))
        })?;
        // The child holds the slave now; the reader sees end of file only
        // once every holder closed it.
        drop(pair.slave);
        // The PTY child is a session leader, so its pid is its group id.
        let Some(pid) = child.process_id() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProtocolError::process_failed("spawned process has no pid"));
        };
        let initial = match self.input {
            ExecInput::Bytes { data } => Some(data.into_inner()),
            ExecInput::None {} | ExecInput::Pipe {} => None,
        };
        let input = Input::new(
            InputSink::Pty(writer),
            initial,
            false,
            true,
            input_queue_bytes,
        );
        Ok(Execution::start_pty(
            pid, child, master, reader, input, sink, redactions, options,
        ))
    }
}

#[cfg(test)]
mod tests {
    use environment_protocol::{
        bytes::ByteChunk,
        error::{ErrorData, ProtocolErrorKind},
    };

    use super::*;

    fn spec(argv: &[&str]) -> CommandSpec {
        CommandSpec {
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            input: ExecInput::Bytes {
                data: ByteChunk::from(b"x".as_slice()),
            },
            timeout_ms: None,
        }
    }

    fn field(error: &ProtocolError) -> Option<&str> {
        match &error.data {
            ErrorData::InvalidParams { field, .. } => field.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn commands_are_validated_with_wire_field_paths() {
        assert!(validate_command(&spec(&["true"]), "command").is_ok());
        let error = validate_command(&spec(&[]), "jobs[1].command").unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
        assert_eq!(field(&error), Some("jobs[1].command.argv"));

        let mut collides = spec(&["true"]);
        collides.env.insert("TOKEN".to_owned(), "plain".to_owned());
        collides
            .secret_env
            .insert("TOKEN".to_owned(), SecretString::new("secret"));
        let error = validate_command(&collides, "command").unwrap_err();
        assert_eq!(field(&error), Some("command.secretEnv"));

        let mut bad_name = spec(&["true"]);
        bad_name.env.insert("A=B".to_owned(), "x".to_owned());
        assert_eq!(
            field(&validate_command(&bad_name, "command").unwrap_err()),
            Some("command.env")
        );

        let mut zero = spec(&["true"]);
        zero.timeout_ms = Some(0);
        assert_eq!(
            field(&validate_command(&zero, "command").unwrap_err()),
            Some("command.timeoutMs")
        );
    }
}
