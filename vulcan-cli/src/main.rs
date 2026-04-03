use serde_json::json;
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    match vulcan_cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if wants_json_output() {
                println!(
                    "{}",
                    json!({
                        "error": error.to_string(),
                        "code": error.code(),
                    })
                );
            } else {
                eprintln!("{error}");
            }
            ExitCode::from(error.exit_code())
        }
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
