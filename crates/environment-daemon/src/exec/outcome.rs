//! How a root process ended, and what its group left behind.

use environment_protocol::{
    data::exec::{ExitStatus, LeftoverDisposition, LeftoverProcess, LeftoverReport, SignalName},
    limits::{MAX_LEFTOVER_COMMAND_BYTES, MAX_LEFTOVER_PROCESSES},
};

/// The wire exit of a reaped child: its status code, or the signal that
/// killed it by name.
pub(crate) fn exit_status(status: std::process::ExitStatus) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return ExitStatus::Signal {
                signal: signal_name(signal),
                core_dumped: status.core_dumped(),
            };
        }
    }
    ExitStatus::Code {
        code: status.code().unwrap_or(-1),
    }
}

/// `exit 3` or `SIGSEGV (core dumped)`, for error messages.
pub(crate) fn describe_exit(exit: &ExitStatus) -> String {
    match exit {
        ExitStatus::Code { code } => format!("exited with status {code}"),
        ExitStatus::Signal {
            signal,
            core_dumped: true,
        } => format!("terminated by signal {signal} (core dumped)"),
        ExitStatus::Signal { signal, .. } => format!("terminated by signal {signal}"),
    }
}

/// The POSIX name of signal number `raw` on this platform. A number
/// without a portable name is spelled `SIG<number>`.
pub(crate) fn signal_name(raw: i32) -> SignalName {
    #[cfg(unix)]
    {
        use rustix::process::Signal;
        let known: &[(Signal, &str)] = &[
            (Signal::HUP, "SIGHUP"),
            (Signal::INT, "SIGINT"),
            (Signal::QUIT, "SIGQUIT"),
            (Signal::ILL, "SIGILL"),
            (Signal::TRAP, "SIGTRAP"),
            (Signal::ABORT, "SIGABRT"),
            (Signal::BUS, "SIGBUS"),
            (Signal::FPE, "SIGFPE"),
            (Signal::KILL, "SIGKILL"),
            (Signal::USR1, "SIGUSR1"),
            (Signal::SEGV, "SIGSEGV"),
            (Signal::USR2, "SIGUSR2"),
            (Signal::PIPE, "SIGPIPE"),
            (Signal::ALARM, "SIGALRM"),
            (Signal::TERM, "SIGTERM"),
            (Signal::CHILD, "SIGCHLD"),
            (Signal::CONT, "SIGCONT"),
            (Signal::STOP, "SIGSTOP"),
            (Signal::TSTP, "SIGTSTP"),
            (Signal::TTIN, "SIGTTIN"),
            (Signal::TTOU, "SIGTTOU"),
            (Signal::URG, "SIGURG"),
            (Signal::XCPU, "SIGXCPU"),
            (Signal::XFSZ, "SIGXFSZ"),
            (Signal::VTALARM, "SIGVTALRM"),
            (Signal::PROF, "SIGPROF"),
            (Signal::WINCH, "SIGWINCH"),
            (Signal::IO, "SIGIO"),
            (Signal::SYS, "SIGSYS"),
            #[cfg(target_os = "linux")]
            (Signal::STKFLT, "SIGSTKFLT"),
            #[cfg(target_os = "linux")]
            (Signal::POWER, "SIGPWR"),
            #[cfg(target_os = "macos")]
            (Signal::EMT, "SIGEMT"),
            #[cfg(target_os = "macos")]
            (Signal::INFO, "SIGINFO"),
        ];
        if let Some((_, name)) = known.iter().find(|(signal, _)| signal.as_raw() == raw) {
            return SignalName::new(*name).expect("known signal names are valid");
        }
    }
    SignalName::new(format!("SIG{}", raw.unsigned_abs()))
        .expect("SIG followed by at most 10 digits is a valid signal name")
}

/// A report from enumerated members, lowest pid first: at most
/// `MAX_LEFTOVER_PROCESSES` listed, commands cut to
/// `MAX_LEFTOVER_COMMAND_BYTES` on a character boundary.
pub(crate) fn leftover_report(
    mut members: Vec<(u32, Option<String>)>,
    disposition: LeftoverDisposition,
) -> Option<LeftoverReport> {
    if members.is_empty() {
        return None;
    }
    members.sort_by_key(|(pid, _)| *pid);
    let listed = MAX_LEFTOVER_PROCESSES as usize;
    let omitted_process_count =
        u32::try_from(members.len().saturating_sub(listed)).unwrap_or(u32::MAX);
    members.truncate(listed);
    let processes = members
        .into_iter()
        .map(|(pid, command)| LeftoverProcess {
            pid,
            command: command
                .map(|command| cut(command, MAX_LEFTOVER_COMMAND_BYTES as usize))
                .filter(|command| !command.is_empty()),
        })
        .collect();
    Some(LeftoverReport {
        disposition,
        processes,
        omitted_process_count,
    })
}

fn cut(mut text: String, max: usize) -> String {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn signal_deaths_are_named_and_codes_kept() {
        use std::os::unix::process::ExitStatusExt as _;
        // A raw wait status: low 7 bits the signal, 0x80 the core flag.
        let segv =
            std::process::ExitStatus::from_raw(rustix::process::Signal::SEGV.as_raw() | 0x80);
        assert_eq!(
            exit_status(segv),
            ExitStatus::Signal {
                signal: SignalName::new("SIGSEGV").unwrap(),
                core_dumped: true,
            }
        );
        let term = std::process::ExitStatus::from_raw(rustix::process::Signal::TERM.as_raw());
        assert_eq!(
            describe_exit(&exit_status(term)),
            "terminated by signal SIGTERM"
        );
        let code = std::process::ExitStatus::from_raw(3 << 8);
        assert_eq!(exit_status(code), ExitStatus::Code { code: 3 });
        assert_eq!(signal_name(200).as_str(), "SIG200");
    }

    #[test]
    fn leftover_reports_are_capped_sorted_and_cut() {
        let members = (0..100u32)
            .rev()
            .map(|pid| (pid + 10, Some("é".repeat(1000))))
            .collect();
        let report = leftover_report(members, LeftoverDisposition::Kept).expect("report");
        assert_eq!(report.processes.len(), MAX_LEFTOVER_PROCESSES as usize);
        assert_eq!(report.omitted_process_count, 100 - MAX_LEFTOVER_PROCESSES);
        assert_eq!(report.processes[0].pid, 10);
        let command = report.processes[0].command.as_deref().unwrap();
        assert!(command.len() <= MAX_LEFTOVER_COMMAND_BYTES as usize);
        assert!(command.len() >= MAX_LEFTOVER_COMMAND_BYTES as usize - 1);
        assert!(leftover_report(Vec::new(), LeftoverDisposition::Swept).is_none());
        let unnamed = leftover_report(vec![(5, Some(String::new()))], LeftoverDisposition::Swept)
            .expect("report");
        assert_eq!(unnamed.processes[0].command, None);
    }
}
