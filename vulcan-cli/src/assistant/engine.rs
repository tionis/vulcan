use crate::assistant::rpc::ManagedRpcClient;
use crate::assistant::AssistantHostOptions;
use crate::CliError;
use serde::Serialize;
use std::env;
use std::ffi::OsString;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub(crate) type EngineRpcClient = ManagedRpcClient<BufReader<ChildStdout>, ChildStdin>;

pub(crate) struct ManagedEngineProcess {
    #[allow(dead_code)]
    child: Child,
    pub(crate) client: EngineRpcClient,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EngineDoctorReport {
    pub(crate) runtime: String,
    pub(crate) configured_binary: String,
    pub(crate) resolved_binary: Option<String>,
    pub(crate) available: bool,
    pub(crate) launch_args: Vec<String>,
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EngineLaunch {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
}

pub(crate) fn doctor(options: &AssistantHostOptions, vault_root: &Path) -> EngineDoctorReport {
    if options.runtime != "pi" {
        return EngineDoctorReport {
            runtime: options.runtime.clone(),
            configured_binary: options.pi_binary.clone(),
            resolved_binary: None,
            available: false,
            launch_args: Vec::new(),
            notes: vec![format!(
                "runtime {:?} is configured; only pi is currently supported",
                options.runtime
            )],
        };
    }

    let resolved = resolve_binary(&options.pi_binary);
    let launch_args = resolved.as_ref().map_or_else(Vec::new, |_| {
        build_pi_launch(options, vault_root)
            .args
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    });
    let mut notes = Vec::new();
    if resolved.is_none() {
        notes.push(
            "pi binary was not found; set [assistant].pi_binary or --assistant-pi-binary"
                .to_string(),
        );
    }
    if options.no_tools {
        notes.push("tool injection is disabled for this launch".to_string());
    }

    EngineDoctorReport {
        runtime: options.runtime.clone(),
        configured_binary: options.pi_binary.clone(),
        resolved_binary: resolved.map(|path| path.display().to_string()),
        available: notes.iter().all(|note| !note.contains("not found")),
        launch_args,
        notes,
    }
}

pub(crate) fn spawn_pi_rpc(
    options: &AssistantHostOptions,
    vault_root: &Path,
) -> Result<ManagedEngineProcess, CliError> {
    let launch = build_pi_launch(options, vault_root);
    let mut child = Command::new(&launch.program)
        .args(&launch.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(CliError::operation)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| CliError::operation("managed engine stdin was not available"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::operation("managed engine stdout was not available"))?;
    Ok(ManagedEngineProcess {
        child,
        client: ManagedRpcClient::new(BufReader::new(stdout), stdin),
    })
}

pub(crate) fn build_pi_launch(options: &AssistantHostOptions, vault_root: &Path) -> EngineLaunch {
    let program =
        resolve_binary(&options.pi_binary).unwrap_or_else(|| PathBuf::from(&options.pi_binary));
    let mut args = vec![
        OsString::from("--mode"),
        OsString::from("rpc"),
        OsString::from("--cwd"),
        vault_root.as_os_str().to_os_string(),
    ];
    if let Some(provider) = options.provider.as_ref() {
        args.push(OsString::from("--provider"));
        args.push(OsString::from(provider));
    }
    if let Some(model) = options.model.as_ref() {
        args.push(OsString::from("--model"));
        args.push(OsString::from(model));
    }
    if let Some(thinking) = options.thinking_level.as_ref() {
        args.push(OsString::from("--thinking"));
        args.push(OsString::from(thinking));
    }
    if let Some(session_dir) = options.resolved_sessions_dir(vault_root) {
        args.push(OsString::from("--session-dir"));
        args.push(session_dir.into_os_string());
    } else {
        args.push(OsString::from("--no-session"));
    }
    EngineLaunch { program, args }
}

fn resolve_binary(binary: &str) -> Option<PathBuf> {
    let path = Path::new(binary);
    if path.components().count() > 1 || path.is_absolute() {
        return path.exists().then(|| path.to_path_buf());
    }
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(binary))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> AssistantHostOptions {
        AssistantHostOptions {
            runtime: "pi".to_string(),
            pi_binary: "pi".to_string(),
            provider: Some("openai".to_string()),
            model: Some("gpt-5.2".to_string()),
            thinking_level: Some("high".to_string()),
            permission_profile: Some("readonly".to_string()),
            sessions_dir: Some(PathBuf::from("AI/Sessions")),
            no_tools: false,
        }
    }

    #[test]
    fn pi_launch_includes_rpc_cwd_model_and_session_dir() {
        let launch = build_pi_launch(&options(), Path::new("/vault"));
        let args = launch
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args[0..4], ["--mode", "rpc", "--cwd", "/vault"]);
        assert!(args.windows(2).any(|pair| pair == ["--provider", "openai"]));
        assert!(args.windows(2).any(|pair| pair == ["--model", "gpt-5.2"]));
        assert!(args.windows(2).any(|pair| pair == ["--thinking", "high"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--session-dir", "/vault/AI/Sessions"]));
    }

    #[test]
    fn pi_launch_can_be_ephemeral() {
        let mut options = options();
        options.sessions_dir = None;
        let args = build_pi_launch(&options, Path::new("/vault"))
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.iter().any(|arg| arg == "--no-session"));
        assert!(!args.iter().any(|arg| arg == "--session-dir"));
    }

    #[test]
    fn doctor_reports_unsupported_runtime_without_resolving_pi() {
        let mut options = options();
        options.runtime = "none".to_string();
        let report = doctor(&options, Path::new("/vault"));

        assert!(!report.available);
        assert!(report
            .notes
            .iter()
            .any(|note| note.contains("only pi is currently supported")));
    }
}
