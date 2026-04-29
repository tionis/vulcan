use serde_json::json;
use std::any::Any;
use std::env;
use std::panic;
use std::process::ExitCode;

fn main() -> ExitCode {
    install_broken_pipe_panic_hook();
    match panic::catch_unwind(real_main) {
        Ok(code) => code,
        Err(payload) => {
            if is_broken_pipe_panic_payload(payload.as_ref()) {
                ExitCode::SUCCESS
            } else {
                panic::resume_unwind(payload)
            }
        }
    }
}

fn real_main() -> ExitCode {
    match vulcan_cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = error.to_string();
            if !message.is_empty() {
                if wants_json_output() {
                    println!(
                        "{}",
                        json!({
                            "error": message,
                            "code": error.code(),
                        })
                    );
                } else {
                    eprintln!("{message}");
                }
            }
            ExitCode::from(error.exit_code())
        }
    }
}

fn install_broken_pipe_panic_hook() {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        if is_broken_pipe_panic_payload(panic_info.payload()) {
            return;
        }
        default_hook(panic_info);
    }));
}

fn is_broken_pipe_panic_payload(payload: &(dyn Any + Send)) -> bool {
    extract_panic_message(payload).is_some_and(|message| {
        message.contains("failed printing to stdout")
            && (message.contains("Broken pipe")
                || message.contains("os error 32")
                || message.contains("The pipe has been ended.")
                || message.contains("os error 109")
                || message.contains("The pipe is being closed.")
                || message.contains("os error 232"))
    })
}

fn extract_panic_message(payload: &(dyn Any + Send)) -> Option<&str> {
    if let Some(message) = payload.downcast_ref::<&str>() {
        Some(message)
    } else if let Some(message) = payload.downcast_ref::<String>() {
        Some(message.as_str())
    } else {
        None
    }
}

fn wants_json_output() -> bool {
    let mut args = env::args_os().skip(1);
    while let Some(argument) = args.next() {
        let rendered = argument.to_string_lossy();
        if rendered == "--output" {
            return args
                .next()
                .is_some_and(|value| value.to_string_lossy() == "json");
        }
        if let Some((flag, value)) = rendered.split_once('=') {
            if flag == "--output" {
                return value == "json";
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_broken_pipe_panic_payload;

    #[test]
    fn detects_broken_pipe_panic_payloads_across_platform_variants() {
        let linux = String::from("failed printing to stdout: Broken pipe (os error 32)");
        let windows_ended =
            String::from("failed printing to stdout: The pipe has been ended. (os error 109)");
        let windows_closed =
            String::from("failed printing to stdout: The pipe is being closed. (os error 232)");

        assert!(is_broken_pipe_panic_payload(&linux));
        assert!(is_broken_pipe_panic_payload(&windows_ended));
        assert!(is_broken_pipe_panic_payload(&windows_closed));
    }

    #[test]
    fn ignores_non_broken_pipe_panic_payloads() {
        let unrelated = String::from("failed printing to stdout: permission denied (os error 5)");
        let stderr = String::from("failed printing to stderr: Broken pipe (os error 32)");

        assert!(!is_broken_pipe_panic_payload(&unrelated));
        assert!(!is_broken_pipe_panic_payload(&stderr));
    }
}
