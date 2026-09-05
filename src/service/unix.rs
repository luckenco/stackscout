use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{Shared, State, protocol};

fn endpoint(root: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(io::Error::other("workspace root must be a directory"));
    }
    if root.to_str().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace root must be UTF-8",
        ));
    }
    let socket = root.join(".stackscout/service.sock");
    Ok((root, socket))
}

/// Runs one foreground service for a canonical workspace root until terminated.
///
/// Ownership is protected by an OS file lock held for the process lifetime. A competing
/// owner returns `WouldBlock` before scanning. Stale sockets are removed only under that lock.
/// Unix socket path-length limits apply; no TCP fallback is implemented yet.
pub fn serve(root: &Path) -> io::Result<()> {
    let (root, socket) = endpoint(root)?;
    let directory = socket.parent().expect("socket has a parent");
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(directory)?;
            if !metadata.is_dir() || metadata.permissions().readonly() {
                return Err(io::Error::other(
                    "service directory must be a writable, real directory",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let lock_path = directory.join("owner.lock");
    if let Ok(metadata) = fs::symlink_metadata(&lock_path)
        && !metadata.file_type().is_file()
    {
        return Err(io::Error::other("service lock must be a regular file"));
    }
    let owner = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    owner.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "service already owned; reconnect",
        ),
        fs::TryLockError::Error(error) => error,
    })?;
    match fs::symlink_metadata(&socket) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(&socket)?,
        Ok(_) => {
            return Err(io::Error::other(
                "refusing to remove a non-socket service endpoint",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let shared = Shared {
        root,
        instance: format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos()
        ),
        state: Mutex::new(State::default()),
    };
    let repo = crate::Repository::new().map_err(io::Error::other)?;
    let (updates, receiver) = mpsc::sync_channel(8);
    thread::scope(|scope| {
        scope.spawn(|| super::worker(&shared, repo, receiver));
        // Bound resource use while allowing two readers alongside an update request.
        for _ in 0..4 {
            let listener = &listener;
            let shared = &shared;
            let updates = &updates;
            scope.spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let _ = connection(stream, shared, updates);
                        }
                        Err(error) => eprintln!("stackscout service accept: {error}"),
                    }
                }
            });
        }
    });
    drop(owner);
    Ok(())
}

fn connection(
    mut stream: UnixStream,
    shared: &Shared,
    updates: &mpsc::SyncSender<super::Update>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::new();
    BufReader::new((&stream).take(protocol::MAX_REQUEST + 1)).read_until(b'\n', &mut bytes)?;
    let response = if bytes.len() as u64 > protocol::MAX_REQUEST {
        shared.failure("request exceeds 64 KiB")
    } else if bytes.last() != Some(&b'\n') {
        shared.failure("request must end with a newline")
    } else {
        super::handle(shared, updates, &bytes)
    };
    serde_json::to_writer(&mut stream, &response)?;
    stream.write_all(b"\n")
}

/// Sends one newline-delimited protocol-v1 JSON request to an existing service.
///
/// This function does not start a process or retry mutations. Responses carry operation
/// errors in their `error` field. Transport failures do not imply that a mutation was canceled;
/// reconnect and inspect the generation. Each call uses a new connection.
pub fn request(root: &Path, json: &str) -> io::Result<String> {
    if json.contains('\n') || json.len() as u64 >= protocol::MAX_REQUEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "request must be one JSON line shorter than 64 KiB",
        ));
    }
    let (root, socket) = endpoint(root)?;
    if !fs::symlink_metadata(socket.parent().expect("socket has a parent"))?.is_dir()
        || !fs::symlink_metadata(&socket)?.file_type().is_socket()
    {
        return Err(io::Error::other(
            "service endpoint must be a real socket in a real directory",
        ));
    }
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(json.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut bytes = Vec::new();
    BufReader::new(stream.take(protocol::MAX_RESPONSE + 1)).read_until(b'\n', &mut bytes)?;
    if bytes.is_empty()
        || bytes.last() != Some(&b'\n')
        || bytes.len() as u64 > protocol::MAX_RESPONSE
    {
        return Err(io::Error::other(
            "missing, incomplete, or oversized service response",
        ));
    }
    // Reject corrupt replies rather than returning arbitrary socket data as a graph result.
    let response: serde_json::Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if response["version"] != protocol::VERSION
        || response["root"].as_str() != root.to_str()
        || !response["instance"].is_string()
    {
        return Err(io::Error::other(
            "invalid service identity or protocol version",
        ));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}
