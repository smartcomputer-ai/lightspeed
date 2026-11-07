//! Bounded installation probes. Only observed shells enter the remote catalog.
use crate::exec::{AfterExit, ExecOptions, OutputSink, PreparedCommand};
use environment_protocol::{
    data::{
        exec::{CommandSpec, ExecInput, ExitStatus, OutputStream, StoppedBy},
        platform::{OperatingSystem, PlatformFacts, ShellInfo, ShellKind},
    },
    paths::{EnvironmentPath, PathConvention},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};

pub(crate) fn facts(scrubbed_env: &[String]) -> PlatformFacts {
    static FACTS: OnceLock<PlatformFacts> = OnceLock::new();
    FACTS
        .get_or_init(|| {
            // Runtime construction is synchronous and may itself be called inside
            // Tokio. Probing uses a separate runtime and the ordinary owned spawner.
            let scrubbed_env = scrubbed_env.to_vec();
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("shell probe runtime")
                    .block_on(discover(&scrubbed_env))
            })
            .join()
            .expect("shell probe thread")
        })
        .clone()
}

#[derive(Default)]
struct Capture(Vec<u8>);
impl OutputSink for Capture {
    fn append(&mut self, stream: OutputStream, bytes: &[u8]) {
        if stream == OutputStream::Stdout {
            self.0.extend_from_slice(
                &bytes[..bytes.len().min(4096usize.saturating_sub(self.0.len()))],
            );
        }
    }
}

async fn probe(executable: &Path, args: &[&str], scrubbed_env: &[String]) -> Option<String> {
    let mut scrubbed_env = scrubbed_env.to_vec();
    scrubbed_env.extend(["BASH_ENV".into(), "ENV".into()]);
    let command = PreparedCommand::new(
        CommandSpec {
            argv: std::iter::once(executable.to_str()?.to_owned())
                .chain(args.iter().map(|arg| (*arg).to_owned()))
                .collect(),
            cwd: None,
            env: BTreeMap::new(),
            secret_env: BTreeMap::new(),
            input: ExecInput::None {},
            timeout_ms: None,
        },
        std::env::current_dir().ok()?,
        &scrubbed_env,
        None,
    );
    let execution = command
        .spawn(
            Capture::default(),
            ExecOptions {
                timeout: Some(Duration::from_secs(3)),
                terminate_grace: Duration::ZERO,
                after_exit: AfterExit::Keep,
            },
            4096,
        )
        .ok()?;
    execution.start_input();
    let done = tokio::time::timeout(
        Duration::from_secs(6),
        execution.wait_until(|state| state.settled()),
    )
    .await
    .is_ok();
    execution.stop(StoppedBy::Kill);
    if !done {
        return None;
    }
    execution.with_state(|state| {
        if state.exit() != Some(&ExitStatus::Code { code: 0 })
            || state.error().is_some()
            || state.stopped_by().is_some()
        {
            return None;
        }
        let text = String::from_utf8(state.sink.0.clone()).ok()?;
        let text = text.trim();
        (!text.is_empty() && text.len() <= 256 && !text.contains('\0')).then(|| text.to_owned())
    })
}

fn executable(path: PathBuf, convention: PathConvention) -> Option<(PathBuf, EnvironmentPath)> {
    if !path.is_absolute() || !path.is_file() {
        return None;
    }
    let path = path.canonicalize().ok()?;
    #[cfg(windows)]
    let path = crate::windows_security::local_drive_path(&path).ok()?;
    let wire = EnvironmentPath::from_native(path.to_str()?, convention).ok()?;
    Some((path, wire))
}

fn search(name: &str) -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(name))
                .collect()
        })
        .unwrap_or_default()
}

async fn add(
    facts: &mut PlatformFacts,
    kind: ShellKind,
    candidates: Vec<PathBuf>,
    args: &[&str],
    scrubbed_env: &[String],
) {
    for candidate in candidates {
        let Some((native, wire)) = executable(candidate, facts.path_convention) else {
            continue;
        };
        let Some(version) = probe(&native, args, scrubbed_env).await else {
            continue;
        };
        if version
            .split('.')
            .next()
            .is_none_or(|major| major.parse::<u32>().is_err())
        {
            continue;
        }
        facts.shells.push(ShellInfo {
            kind,
            executable: wire,
            version,
        });
        break;
    }
}

async fn discover(scrubbed_env: &[String]) -> PlatformFacts {
    let os = if cfg!(windows) {
        OperatingSystem::Windows
    } else if cfg!(target_os = "macos") {
        OperatingSystem::Macos
    } else if cfg!(target_os = "linux") {
        OperatingSystem::Linux
    } else {
        OperatingSystem::Other
    };
    let mut facts = PlatformFacts::without_shells(os);
    if cfg!(windows) {
        let mut powershell = search("pwsh.exe");
        if let Some(programs) = std::env::var_os("ProgramFiles") {
            powershell.push(PathBuf::from(programs).join("PowerShell/7/pwsh.exe"));
        }
        add(&mut facts, ShellKind::PowerShell, powershell, &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "if ($PSVersionTable.PSEdition -ne 'Core' -or $PSVersionTable.PSVersion.Major -lt 7) { exit 1 }; [Console]::Write($PSVersionTable.PSVersion.ToString())"], scrubbed_env).await;
        let legacy = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"))
            .into_iter()
            .collect();
        add(&mut facts, ShellKind::WindowsPowerShell, legacy, &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "if ($PSVersionTable.PSEdition -ne 'Desktop' -or $PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -lt 1) { exit 1 }; [Console]::Write($PSVersionTable.PSVersion.ToString())"], scrubbed_env).await;
        // Never accept a PATH bash.exe: it can be a WSL launcher. Verify both
        // Git for Windows and its adjacent Bash installation.
        let mut roots: Vec<_> = ["ProgramFiles", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(|path| PathBuf::from(path).join("Git"))
            .collect();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("Programs/Git"));
        }
        for root in roots {
            let git = root.join("cmd/git.exe");
            if probe(&git, &["--version"], scrubbed_env)
                .await
                .is_some_and(|version| {
                    version.starts_with("git version ") && version.contains(".windows.")
                })
            {
                add(
                    &mut facts,
                    ShellKind::GitBash,
                    vec![root.join("bin/bash.exe")],
                    &[
                        "--noprofile",
                        "--norc",
                        "-c",
                        "printf '%s' \"$BASH_VERSION\"",
                    ],
                    scrubbed_env,
                )
                .await;
                if facts
                    .shells
                    .iter()
                    .any(|shell| shell.kind == ShellKind::GitBash)
                {
                    break;
                }
            }
        }
    } else {
        let mut bash = vec![PathBuf::from("/bin/bash"), PathBuf::from("/usr/bin/bash")];
        bash.extend(search("bash"));
        add(
            &mut facts,
            ShellKind::Bash,
            bash,
            &[
                "--noprofile",
                "--norc",
                "-c",
                "printf '%s' \"$BASH_VERSION\"",
            ],
            scrubbed_env,
        )
        .await;
    }
    facts.default_shell = if cfg!(windows) {
        [ShellKind::PowerShell, ShellKind::WindowsPowerShell]
            .into_iter()
            .find(|kind| facts.shells.iter().any(|shell| shell.kind == *kind))
    } else {
        facts.shells.first().map(|shell| shell.kind)
    };
    facts.validate().expect("probed platform facts");
    facts
}
