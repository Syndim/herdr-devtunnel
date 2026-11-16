use std::ffi::{OsStr, OsString};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use super::Provider;

pub(super) fn detect() -> Option<Provider> {
    let mut system = System::new();
    let mut pid = Pid::from_u32(std::process::id());
    let refresh = ProcessRefreshKind::nothing()
        .without_tasks()
        .with_cmd(UpdateKind::OnlyIfNotSet);
    // Walk ancestors rather than just the parent: hooks can run through a shell.
    // Only read command lines for this chain, not every process on the machine.
    let mut visited = Vec::new();
    for depth in 0..32 {
        if visited.contains(&pid) {
            break;
        }
        visited.push(pid);
        system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh);
        let process = system.process(pid)?;
        if depth > 0
            && let Some(provider) = identify(process.name(), process.cmd())
        {
            return Some(provider);
        }
        pid = process.parent()?;
    }
    None
}

fn basename(value: &OsStr) -> String {
    // Accept either separator so Windows command lines are testable on Unix too.
    value
        .to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn identify(name: &OsStr, cmd: &[OsString]) -> Option<Provider> {
    let executable = cmd
        .first()
        .map_or_else(|| basename(name), |arg| basename(arg));
    if matches!(executable.as_str(), "copilot" | "copilot.exe") {
        return Some(Provider::Copilot);
    }
    if !matches!(executable.as_str(), "node" | "node.exe" | "nodejs") {
        return None;
    }
    // npm installations use Node as the executable. Inspect its script path,
    // never arbitrary prompt arguments or shell command text.
    let script = cmd.get(1)?.to_string_lossy().replace('\\', "/");
    if script.starts_with('-') {
        return None;
    }
    let script_name = basename(OsStr::new(&script));
    if script_name == "copilot"
        || (script.contains("/@github/copilot/")
            && [".js", ".mjs", ".cjs"]
                .iter()
                .any(|suffix| script.ends_with(suffix)))
    {
        Some(Provider::Copilot)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(args: &[&str]) -> Option<Provider> {
        let cmd: Vec<OsString> = args.iter().map(OsString::from).collect();
        identify(OsStr::new("ignored"), &cmd)
    }

    #[test]
    fn recognizes_native_and_node_copilot() {
        for args in [
            vec!["/usr/local/bin/copilot"],
            vec![r"C:\Tools\copilot.exe"],
            vec!["node", "/usr/lib/node_modules/@github/copilot/index.js"],
            vec!["node.exe", r"C:\npm\node_modules\@github\copilot\index.js"],
            vec!["node", "/usr/local/bin/copilot"],
        ] {
            assert_eq!(provider(&args), Some(Provider::Copilot), "{args:?}");
        }
    }

    #[test]
    fn does_not_infer_provider_from_prompts_or_shell_text() {
        for args in [
            vec!["bash", "-c", "copilot"],
            vec!["other-agent", "copilot"],
            vec!["node", "other.js", "/npm/@github/copilot/index.js"],
            vec!["node", "-e", "/npm/@github/copilot/index.js"],
            vec!["copilot-unrelated"],
        ] {
            assert_eq!(provider(&args), None, "{args:?}");
        }
        assert_eq!(
            identify(OsStr::new("copilot.exe"), &[]),
            Some(Provider::Copilot)
        );
        assert_eq!(identify(OsStr::new("unknown"), &[]), None);
    }
}
