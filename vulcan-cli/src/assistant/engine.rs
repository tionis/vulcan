use crate::assistant::rpc::ManagedRpcClient;
use crate::assistant::AssistantHostOptions;
use crate::CliError;
use serde::Serialize;
use serde_json::Map;
use std::env;
use std::ffi::OsString;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) type EngineRpcClient = ManagedRpcClient<BufReader<ChildStdout>, ChildStdin>;

pub(crate) struct ManagedEngineProcess {
    child: Child,
    pub(crate) client: EngineRpcClient,
}

impl ManagedEngineProcess {
    pub(crate) fn is_healthy(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|status| status.is_none())
    }

    pub(crate) fn ensure_running(&mut self) -> Result<(), CliError> {
        if self.is_healthy() {
            Ok(())
        } else {
            Err(CliError::operation(
                "managed assistant engine exited unexpectedly",
            ))
        }
    }

    pub(crate) fn shutdown(mut self) -> Result<(), CliError> {
        let _ = self.client.command("abort", Map::default());
        drop(self.client);
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self
                .child
                .try_wait()
                .map_err(CliError::operation)?
                .is_some()
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.child.kill().map_err(CliError::operation)?;
        let _ = self.child.wait();
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EngineDoctorReport {
    pub(crate) runtime: String,
    pub(crate) configured_binary: String,
    pub(crate) resolved_binary: Option<String>,
    pub(crate) available: bool,
    pub(crate) launch_args: Vec<String>,
    pub(crate) notes: Vec<String>,
    pub(crate) version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EngineLaunch {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) env: Vec<(String, String)>,
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
            version: None,
        };
    }

    let resolved = resolve_binary(&options.pi_binary);
    let version = resolved.as_deref().and_then(pi_version);
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
    if version.is_none() && resolved.is_some() {
        notes.push("could not read pi version with --version".to_string());
    }

    EngineDoctorReport {
        runtime: options.runtime.clone(),
        configured_binary: options.pi_binary.clone(),
        resolved_binary: resolved.map(|path| path.display().to_string()),
        available: notes.iter().all(|note| !note.contains("not found")),
        launch_args,
        notes,
        version,
    }
}

pub(crate) fn spawn_pi_rpc(
    options: &AssistantHostOptions,
    vault_root: &Path,
) -> Result<ManagedEngineProcess, CliError> {
    let launch = build_pi_launch(options, vault_root);
    let mut child = Command::new(&launch.program)
        .args(&launch.args)
        .envs(launch.env.iter().map(|(key, value)| (key, value)))
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
    if let Some(extension) = options.extension_entrypoint.as_ref() {
        args.push(OsString::from("-e"));
        args.push(extension.as_os_str().to_os_string());
    }
    if let Some(session) = options.resume_session.as_ref() {
        args.push(OsString::from("--session"));
        args.push(session.as_os_str().to_os_string());
    }
    if let Some(session_dir) = options.resolved_sessions_dir(vault_root) {
        args.push(OsString::from("--session-dir"));
        args.push(session_dir.into_os_string());
    } else {
        args.push(OsString::from("--no-session"));
    }
    EngineLaunch {
        program,
        args,
        env: options.extension_env.clone(),
    }
}

pub(crate) fn resolve_binary(binary: &str) -> Option<PathBuf> {
    if let Some(from_env) = env::var_os("PI_BINARY") {
        let candidate = PathBuf::from(from_env);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let path = Path::new(binary);
    if path.components().count() > 1 || path.is_absolute() {
        return path.exists().then(|| path.to_path_buf());
    }
    let from_path = env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(binary))
            .find(|candidate| candidate.is_file())
    });
    from_path.or_else(|| {
        let home = env::var_os("HOME").map(PathBuf::from);
        [
            home.as_ref().map(|home| home.join(".npm-global/bin/pi")),
            Some(PathBuf::from("/usr/local/bin/pi")),
        ]
        .into_iter()
        .flatten()
        .find(|candidate| candidate.is_file())
    })
}

fn pi_version(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stdout.is_empty() {
        (!stderr.is_empty()).then_some(stderr)
    } else {
        Some(stdout)
    }
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
            extension_entrypoint: None,
            extension_env: Vec::new(),
            resume_session: None,
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
    fn pi_launch_includes_extension_and_session_resume() {
        let mut options = options();
        options.extension_entrypoint = Some(PathBuf::from("/vault/.vulcan/assistant/index.ts"));
        options.resume_session = Some(PathBuf::from("/vault/AI/Sessions/last.jsonl"));
        options.extension_env = vec![("VULCAN_VAULT_ROOT".to_string(), "/vault".to_string())];
        let launch = build_pi_launch(&options, Path::new("/vault"));
        let args = launch
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args
            .windows(2)
            .any(|pair| pair == ["-e", "/vault/.vulcan/assistant/index.ts"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--session", "/vault/AI/Sessions/last.jsonl"]));
        assert_eq!(
            launch.env,
            vec![("VULCAN_VAULT_ROOT".to_string(), "/vault".to_string())]
        );
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
