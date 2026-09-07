use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run(env::args_os().skip(1).collect()) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("stackscout: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<std::ffi::OsString>) -> Result<String, String> {
    #[cfg(unix)]
    match arguments.as_slice() {
        [command, root] if command == "serve" => {
            stackscout::service::serve(&PathBuf::from(root)).map_err(|error| error.to_string())?;
            return Ok(String::new());
        }
        [command, root] if command == "connect" => {
            let root = PathBuf::from(root);
            // A read-only handshake owns startup; never send/replay a caller's mutation here.
            request(&root, r#"{"version":1,"op":"status"}"#).map_err(|error| error.to_string())?;
            let (root, socket) =
                stackscout::service::endpoint(&root).map_err(|error| error.to_string())?;
            return Ok(
                serde_json::json!({"version": 1, "root": root, "socket": socket}).to_string()
                    + "\n",
            );
        }
        [command, root, json] if command == "request" => {
            let json = json.to_str().ok_or("request must be UTF-8 JSON")?;
            return request(&PathBuf::from(root), json).map_err(|error| error.to_string());
        }
        _ => {}
    }
    let (before, after, diagnostics) = match arguments.as_slice() {
        [command, before, after] if command == "diff-dir" => (before, after, false),
        [command, flag, before, after] if command == "diff-dir" && flag == "--diagnostics" => {
            (before, after, true)
        }
        _ => return Err(usage()),
    };
    let before = PathBuf::from(before);
    let after = PathBuf::from(after);
    let result = if diagnostics {
        stackscout::diff_dirs_with_diagnostics(before, after)
    } else {
        stackscout::diff_dirs(before, after)
    };
    result.map_err(|error| error.to_string())
}

fn usage() -> String {
    "usage: stackscout diff-dir [--diagnostics] <before-directory> <after-directory>\n       stackscout serve <root> | connect <root> | request <root> '<json>' (Unix only)".to_owned()
}

#[cfg(unix)]
fn request(root: &std::path::Path, json: &str) -> std::io::Result<String> {
    use std::io::ErrorKind;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let root = root.canonicalize()?;
    match stackscout::service::request(&root, json) {
        Ok(response) => return Ok(response),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => return Err(error),
    }
    let mut child = Command::new(env::current_exe()?)
        .arg("serve")
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match stackscout::service::request(&root, json) {
            Ok(response) => {
                let _ = child.try_wait(); // Reap a startup loser if it has already exited.
                return Ok(response);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::ConnectionRefused
                ) && Instant::now() < deadline => {}
            Err(error) => {
                let _ = child.try_wait();
                return Err(error);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
