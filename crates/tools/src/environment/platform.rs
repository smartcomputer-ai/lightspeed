//! Model context describing the selected remote environment.
use environment_protocol::{data::handshake::InitializeResponse, paths::EnvironmentPath};
pub const CONTEXT_KEY: &str = "runtime.catalog.active_environment";

pub fn describe(id: &str, facts: &InitializeResponse, cwd: &EnvironmentPath) -> String {
    let platform = &facts.platform;
    let mut text = format!(
        "Active environment: {id}\nOperating system: {:?}\nWorking directory: {}\n",
        platform.os,
        cwd.to_native()
    );
    if let Some(home) = &facts.home_directory {
        text.push_str(&format!("Execution user home: {}\n", home.to_native()));
    }
    if let Some(default) = platform.default_shell {
        text.push_str(&format!("Default shell: {}\n", shell_name(default)));
    } else {
        text.push_str("No default shell is available. Use direct program argv or explicitly select an available shell.\n");
    }
    for shell in &platform.shells {
        text.push_str(&format!(
            "Shell {}: {} (version {})\n",
            shell_name(shell.kind),
            shell.executable.to_native(),
            shell.version
        ));
    }
    text.push_str("Shell commands use these remote facts, regardless of the controller OS. Direct argv commands do not use a shell. Unavailable shells fail without fallback.\n");
    if platform.os == environment_protocol::data::platform::OperatingSystem::Windows {
        if platform.default_shell.is_some() {
            text.push_str("Use PowerShell syntax for the default shell. ");
        }
        text.push_str("Profiles are off unless login is explicitly true. An installed Git Bash requires shell: gitBash. Use native drive paths for file tools; relative paths resolve against the working directory. Prefer structured file tools for edits. Keep destructive shell operations in one shell. Terminal EOF is unavailable.\n");
    }
    if let Some(process) = &facts.capabilities.process {
        text.push_str(&format!(
            "Process support: terminal={}, pipe interrupt={}, terminal interrupt={}.\n",
            process.pty, process.interrupt.pipe, process.interrupt.pty
        ));
        if process.termination
            == environment_protocol::data::platform::TerminationMode::ImmediateKill
        {
            text.push_str("Terminate or kill stops the owned process tree immediately.\n");
        }
    } else {
        text.push_str("Process execution is unavailable.\n");
    }
    text.push_str(if facts.capabilities.job.is_some() {
        "Durable jobs are available and accept direct program argv.\n"
    } else {
        "Durable jobs are unavailable.\n"
    });
    if !facts
        .capabilities
        .filesystem
        .as_ref()
        .is_some_and(|fs| fs.transfer)
    {
        text.push_str("File/tree transfers are unavailable. File read and write tools are separate capabilities.\n");
    }
    text
}

fn shell_name(kind: environment_protocol::data::platform::ShellKind) -> &'static str {
    use environment_protocol::data::platform::ShellKind::*;
    match kind {
        Bash => "bash",
        PowerShell => "powerShell",
        WindowsPowerShell => "windowsPowerShell",
        GitBash => "gitBash",
    }
}
