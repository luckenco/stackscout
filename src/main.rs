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
    if arguments.len() != 3 || arguments[0] != "diff-dir" {
        return Err("usage: stackscout diff-dir <before-directory> <after-directory>".to_owned());
    }
    let before = PathBuf::from(&arguments[1]);
    let after = PathBuf::from(&arguments[2]);
    stackscout::diff_dirs(before, after).map_err(|error| error.to_string())
}
