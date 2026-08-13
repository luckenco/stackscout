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
    "usage: stackscout diff-dir [--diagnostics] <before-directory> <after-directory>".to_owned()
}
