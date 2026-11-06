//! Observed execution facts. These describe the remote environment, never the
//! controller's host or `ImplementationInfo.target`. Discovery does not grant
//! filesystem or process capabilities.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::paths::{EnvironmentPath, PathConvention};

/// The remote OS. `other` does not imply Unix execution support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum OperatingSystem {
    Linux,
    Macos,
    Windows,
    Other,
}

/// A supported shell identity, not an arbitrary executable supplied by a caller.
/// `gitBash` means verified Git for Windows Bash, never a WSL launcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ShellKind {
    Bash,
    PowerShell,
    WindowsPowerShell,
    GitBash,
}

/// One verified shell installation. Executables use the environment path grammar;
/// versions are observed by the daemon, not inferred from executable names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellInfo {
    pub kind: ShellKind,
    pub executable: EnvironmentPath,
    /// Nonempty observed version, at most 256 UTF-8 bytes.
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 256)))]
    pub version: String,
}

/// Remote platform discovery. An empty shell catalog means no shell has been
/// verified; it is not permission to guess `bash`, `cmd` or PowerShell on PATH.
/// There is at most one selected installation per shell kind. On Windows the
/// default is PowerShell 7 when verified, otherwise Windows PowerShell; Git Bash
/// is available only by explicit selection. No default is allowed when neither
/// PowerShell is available. Unix defaults to verified Bash when available.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformFacts {
    pub os: OperatingSystem,
    pub path_convention: PathConvention,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shells: Vec<ShellInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_shell: Option<ShellKind>,
}

/// Invalid or unavailable execution facts, detected before constructing a command.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PlatformError {
    #[error("OS and path convention disagree")]
    PathConvention,
    #[error("cwd or home path does not match the platform convention")]
    EnvironmentPathConvention,
    #[error("execution capabilities or grace periods disagree with the platform")]
    Lifecycle,
    #[error("shell installation is invalid for this platform: {0:?}")]
    InvalidShell(ShellKind),
    #[error("duplicate shell identity: {0:?}")]
    DuplicateShell(ShellKind),
    #[error("default shell does not follow the platform preference")]
    InvalidDefault,
    #[error("no default shell is available")]
    NoDefaultShell,
    #[error("requested shell is unavailable: {0:?}")]
    ShellUnavailable(ShellKind),
}

impl PlatformFacts {
    /// Platform facts before shell probing. Empty catalogs deliberately advertise
    /// no shell; later discovery must insert only verified installations.
    pub fn without_shells(os: OperatingSystem) -> Self {
        Self {
            os,
            path_convention: if os == OperatingSystem::Windows {
                PathConvention::Windows
            } else {
                PathConvention::Posix
            },
            shells: Vec::new(),
            default_shell: None,
        }
    }

    pub fn validate(&self) -> Result<(), PlatformError> {
        if (self.os == OperatingSystem::Windows)
            != (self.path_convention == PathConvention::Windows)
        {
            return Err(PlatformError::PathConvention);
        }
        for (index, shell) in self.shells.iter().enumerate() {
            let windows_only = matches!(
                shell.kind,
                ShellKind::WindowsPowerShell | ShellKind::GitBash
            );
            if shell.executable.convention() != self.path_convention
                || shell.executable.is_root()
                || shell.version.trim().is_empty()
                || shell.version.len() > 256
                || shell.version.contains('\0')
                || (windows_only && self.os != OperatingSystem::Windows)
                || (shell.kind == ShellKind::Bash && self.os == OperatingSystem::Windows)
            {
                return Err(PlatformError::InvalidShell(shell.kind));
            }
            if self.shells[..index]
                .iter()
                .any(|other| other.kind == shell.kind)
            {
                return Err(PlatformError::DuplicateShell(shell.kind));
            }
        }
        let preferences: &[ShellKind] = if self.os == OperatingSystem::Windows {
            &[ShellKind::PowerShell, ShellKind::WindowsPowerShell]
        } else {
            &[ShellKind::Bash]
        };
        let expected = preferences
            .iter()
            .copied()
            .find(|kind| self.shells.iter().any(|shell| shell.kind == *kind));
        if self.default_shell != expected {
            return Err(PlatformError::InvalidDefault);
        }
        Ok(())
    }

    /// Resolve an explicit identity or the default. Never switches shell after
    /// an error and never interprets a model-provided string as an executable.
    pub fn shell(&self, requested: Option<ShellKind>) -> Result<&ShellInfo, PlatformError> {
        self.validate()?;
        let kind = requested
            .or(self.default_shell)
            .ok_or(PlatformError::NoDefaultShell)?;
        self.shells
            .iter()
            .find(|shell| shell.kind == kind)
            .ok_or(PlatformError::ShellUnavailable(kind))
    }
}

/// Ownership model of a process or job. This describes what tree membership
/// means; it must not be inferred from a numeric PID or a build target string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum ProcessTree {
    /// Own process group. Descendants may escape with setsid. On daemon restart,
    /// identify surviving groups using boot/creation identity; adopt process
    /// leftovers and sweep jobs. No guarantee of cleanup at daemon death.
    PosixProcessGroup,
    /// Non-breakaway Job Object attached before execution, owned by the daemon
    /// with non-inheritable handles and kill-on-close. Natural process root exit
    /// keeps the job handle and surviving descendants; natural job root exit
    /// sweeps descendants. Explicit close, timeout, cancellation and daemon death
    /// terminate the owned tree. Restart never adopts a raw persisted PID.
    /// Refuse spawn if atomic containment/nested-job constraints cannot be met.
    WindowsJobObject,
}

/// Interrupt delivery mechanisms, separately for pipes and terminals. Missing
/// flags mean `capabilityUnavailable`, never forced kill presented as interrupt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterruptCapabilities {
    #[serde(default, skip_serializing_if = "crate::serde_util::is_false")]
    pub pipe: bool,
    #[serde(default, skip_serializing_if = "crate::serde_util::is_false")]
    pub pty: bool,
}

/// How semantic termination works. Kill always forces the whole owned group/job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TerminationMode {
    /// Send a graceful request, then force termination after the advertised grace.
    GracefulThenKill,
    /// No graceful mechanism; terminate/cancel/timeout immediately force the tree.
    /// The advertised grace must be zero. Never claims a Unix signal was sent.
    ImmediateKill,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(kind: ShellKind, path: &str) -> ShellInfo {
        ShellInfo {
            kind,
            executable: EnvironmentPath::new(path).unwrap(),
            version: "7.5".into(),
        }
    }

    #[test]
    fn powershell_preference_and_explicit_git_bash_are_platform_facts() {
        let mut facts = PlatformFacts::without_shells(OperatingSystem::Windows);
        assert_eq!(facts.shell(None), Err(PlatformError::NoDefaultShell));
        facts
            .shells
            .push(shell(ShellKind::GitBash, "C:/Git/bin/bash.exe"));
        assert_eq!(facts.shell(None), Err(PlatformError::NoDefaultShell));
        assert_eq!(
            facts.shell(Some(ShellKind::GitBash)).unwrap().kind,
            ShellKind::GitBash
        );
        facts.shells.push(shell(
            ShellKind::WindowsPowerShell,
            "C:/Windows/powershell.exe",
        ));
        facts.default_shell = Some(ShellKind::WindowsPowerShell);
        assert_eq!(
            facts.shell(None).unwrap().kind,
            ShellKind::WindowsPowerShell
        );
        facts
            .shells
            .push(shell(ShellKind::PowerShell, "C:/PowerShell/pwsh.exe"));
        assert_eq!(facts.validate(), Err(PlatformError::InvalidDefault));
        facts.default_shell = Some(ShellKind::PowerShell);
        assert_eq!(facts.shell(None).unwrap().kind, ShellKind::PowerShell);
        assert_eq!(
            facts
                .shell(Some(ShellKind::WindowsPowerShell))
                .unwrap()
                .kind,
            ShellKind::WindowsPowerShell
        );
    }

    #[test]
    fn inconsistent_shell_catalogs_cannot_construct_commands() {
        let mut facts = PlatformFacts::without_shells(OperatingSystem::Windows);
        facts.path_convention = PathConvention::Posix;
        assert_eq!(facts.validate(), Err(PlatformError::PathConvention));
        facts.path_convention = PathConvention::Windows;
        facts
            .shells
            .push(shell(ShellKind::Bash, "C:/Windows/bash.exe"));
        assert_eq!(
            facts.validate(),
            Err(PlatformError::InvalidShell(ShellKind::Bash))
        );
        facts.shells = vec![shell(ShellKind::PowerShell, "/usr/bin/pwsh")];
        assert_eq!(
            facts.validate(),
            Err(PlatformError::InvalidShell(ShellKind::PowerShell))
        );
        facts.shells = vec![shell(ShellKind::PowerShell, "C:/pwsh.exe"); 2];
        assert_eq!(
            facts.validate(),
            Err(PlatformError::DuplicateShell(ShellKind::PowerShell))
        );
    }

    #[test]
    fn discovery_and_lifecycle_shapes_are_explicit_and_strict() {
        let windows = serde_json::json!({"os": "windows", "pathConvention": "windows"});
        let facts: PlatformFacts = serde_json::from_value(windows.clone()).unwrap();
        facts.validate().unwrap();
        assert_eq!(serde_json::to_value(facts).unwrap(), windows);
        let mut invalid = windows;
        invalid["target"] = serde_json::json!("guess-from-this");
        assert!(serde_json::from_value::<PlatformFacts>(invalid).is_err());
        let process: super::super::handshake::ProcessCapabilities = serde_json::from_value(serde_json::json!({
            "tree": "windowsJobObject", "termination": "immediateKill", "interrupt": {}, "pty": true
        })).unwrap();
        assert_eq!(process.tree, ProcessTree::WindowsJobObject);
        assert_eq!(process.termination, TerminationMode::ImmediateKill);
        assert!(!process.interrupt.pipe && !process.interrupt.pty);
        assert!(
            serde_json::from_value::<super::super::handshake::ProcessCapabilities>(
                serde_json::json!({"pty": true})
            )
            .is_err()
        );
    }
}
