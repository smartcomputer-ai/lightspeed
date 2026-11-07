//! Pure shell adapters for a discovered remote environment. Direct program argv
//! never passes through these adapters. Model-facing tools opt into them when
//! their platform integration is ready.

use environment_protocol::data::platform::{PlatformError, PlatformFacts, ShellKind};
use thiserror::Error;

/// Model-facing shell request. `shell` selects a catalog identity, never a path.
/// Omitted `login` preserves Unix Bash's historical login default. Windows
/// PowerShell and Git Bash omit profiles by default; explicit true loads them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShellOptions {
    pub shell: Option<ShellKind>,
    pub login: Option<bool>,
    pub tty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ShellError {
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error("shell command contains a NUL character")]
    NulCommand,
}

/// Build program plus arguments using remote facts. Script text remains one
/// argument; no argv flattening or universal shell escaping is involved.
/// PowerShell native failures preserve the final failing command's exit code;
/// script/parse failures return nonzero. Explicit script `exit` remains in charge.
/// PowerShell console/native-pipeline text uses UTF-8 without BOM; file cmdlets
/// still follow that shell version's own encoding rules.
/// CMD/batch adapters are not supported: execute native programs directly or
/// select a verified shell explicitly. No shell-error fallback is permitted.
pub fn command_argv(
    platform: &PlatformFacts,
    command: &str,
    options: ShellOptions,
) -> Result<Vec<String>, ShellError> {
    if command.contains('\0') {
        return Err(ShellError::NulCommand);
    }
    let shell = platform.shell(options.shell)?;
    let mut argv = vec![shell.executable.to_native()];
    match shell.kind {
        ShellKind::Bash | ShellKind::GitBash => {
            let login = options.login.unwrap_or(shell.kind == ShellKind::Bash);
            argv.push(if login { "-lc" } else { "-c" }.into());
            argv.push(command.into());
        }
        ShellKind::PowerShell | ShellKind::WindowsPowerShell => {
            argv.push("-NoLogo".into());
            if !options.login.unwrap_or(false) {
                argv.push("-NoProfile".into());
            }
            if !options.tty {
                argv.push("-NonInteractive".into());
            }
            argv.push("-Command".into());
            argv.push(format!(
                "$OutputEncoding = [Console]::InputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)\n\
                 $global:LASTEXITCODE = 0\n{command}\n\
                 if (-not $?) {{ if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}; exit 1 }}\nexit 0"
            ));
        }
    }
    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment_protocol::{
        data::platform::{OperatingSystem, ShellInfo},
        paths::EnvironmentPath,
    };

    fn windows() -> PlatformFacts {
        let mut facts = PlatformFacts::without_shells(OperatingSystem::Windows);
        facts.shells.push(ShellInfo {
            kind: ShellKind::WindowsPowerShell,
            executable: EnvironmentPath::new(
                "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
            )
            .unwrap(),
            version: "5.1".into(),
        });
        facts.default_shell = Some(ShellKind::WindowsPowerShell);
        facts
    }

    #[test]
    fn remote_windows_uses_powershell_without_profiles_or_bash_fallback() {
        let facts = windows();
        let script = "& 'C:\\Program Files\\tool.exe' '' '雪'\nWrite-Output \"quoted\"";
        let argv = command_argv(&facts, script, ShellOptions::default()).unwrap();
        assert_eq!(
            &argv[1..5],
            ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
        );
        assert!(argv[0].starts_with("C:\\Windows\\"));
        assert!(argv[5].contains(script));
        assert_eq!(
            command_argv(
                &facts,
                "echo hi",
                ShellOptions {
                    shell: Some(ShellKind::GitBash),
                    ..Default::default()
                }
            ),
            Err(ShellError::Platform(PlatformError::ShellUnavailable(
                ShellKind::GitBash
            )))
        );
        let interactive = command_argv(
            &facts,
            script,
            ShellOptions {
                login: Some(true),
                tty: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(&interactive[1..3], ["-NoLogo", "-Command"]);
    }

    #[test]
    fn bash_profile_defaults_are_preserved_and_git_bash_is_explicit() {
        let mut facts = PlatformFacts::without_shells(OperatingSystem::Linux);
        facts.shells.push(ShellInfo {
            kind: ShellKind::Bash,
            executable: EnvironmentPath::new("/bin/bash").unwrap(),
            version: "5.2".into(),
        });
        facts.default_shell = Some(ShellKind::Bash);
        assert_eq!(
            command_argv(&facts, "echo '$x'", ShellOptions::default()).unwrap(),
            ["/bin/bash", "-lc", "echo '$x'"]
        );
        assert_eq!(
            command_argv(
                &facts,
                "",
                ShellOptions {
                    login: Some(false),
                    ..Default::default()
                }
            )
            .unwrap(),
            ["/bin/bash", "-c", ""]
        );
        let mut facts = windows();
        facts.shells.push(ShellInfo {
            kind: ShellKind::GitBash,
            executable: EnvironmentPath::new("C:/Program Files/Git/bin/bash.exe").unwrap(),
            version: "5.2".into(),
        });
        assert_eq!(
            command_argv(
                &facts,
                "printf '%s' /literal",
                ShellOptions {
                    shell: Some(ShellKind::GitBash),
                    ..Default::default()
                }
            )
            .unwrap(),
            [
                "C:\\Program Files\\Git\\bin\\bash.exe",
                "-c",
                "printf '%s' /literal"
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn baseline_powershell_preserves_native_exit_codes_and_rejects_parse_errors() {
        let system_root = std::env::var("SystemRoot").expect("Windows SystemRoot");
        let mut facts = windows();
        facts.shells[0].executable = EnvironmentPath::from_native(
            &format!("{system_root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
            environment_protocol::paths::PathConvention::Windows,
        )
        .unwrap();
        for (script, expected) in [
            ("& $env:ComSpec /d /c 'exit 37'", 37),
            ("Write-Output '雪'", 0),
            ("Write-Error 'failure'", 1),
            ("if (", 1),
        ] {
            let argv = command_argv(&facts, script, ShellOptions::default()).unwrap();
            let output = std::process::Command::new(&argv[0])
                .args(&argv[1..])
                .output()
                .expect("baseline Windows PowerShell must be installed");
            assert_eq!(
                output.status.code(),
                Some(expected),
                "{script}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            if script == "Write-Output '雪'" {
                assert_eq!(
                    String::from_utf8(output.stdout)
                        .expect("UTF-8 output")
                        .trim(),
                    "雪"
                );
            }
        }
    }
}
