use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};

use mio::{Events, Interest, Poll, Token, unix::SourceFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{Shared, State, Update, protocol};

fn uid() -> u32 {
    // SAFETY: geteuid has no arguments, preconditions, or owned resources.
    unsafe { libc::geteuid() }
}

// Filesystem mode checks are defense in depth: ACLs can grant access despite them.
// Authenticate the connected peer before reading requests or sending source evidence.
fn check_peer(stream: &UnixStream, expected: u32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    let peer = {
        let mut credentials = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
        // SAFETY: the descriptor is live and both output buffers have the declared sizes.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut credentials as *mut libc::ucred).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize != std::mem::size_of_val(&credentials) {
            return Err(io::Error::other("invalid peer credentials"));
        }
        credentials.uid
    };
    #[cfg(target_os = "macos")]
    let peer = {
        let mut user = 0;
        let mut group = 0;
        // SAFETY: the descriptor is live and the uid/gid outputs are valid pointers.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut user, &mut group) } != 0 {
            return Err(io::Error::last_os_error());
        }
        user
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let peer: u32 = {
        let _ = (stream, expected);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "peer authentication requires Linux or macOS",
        ));
    };
    if peer != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "service peer must have the owner's effective uid",
        ));
    }
    Ok(())
}

fn paths(root: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(io::Error::other("workspace root must be a directory"));
    }
    let Some(text) = root.to_str() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace root must be UTF-8",
        ));
    };
    let mut socket = root.join(".stackscout/service.sock");
    // Leave room for the NUL on both macOS and Linux; do not depend on Linux abstract sockets.
    if socket.as_os_str().len() >= 100 {
        socket = PathBuf::from(format!("/tmp/stackscout-{}", uid()))
            .join(format!("{}.sock", blake3::hash(text.as_bytes()).to_hex()));
    }
    Ok((root, socket))
}

fn private_directory(path: &Path, create: bool) -> io::Result<()> {
    if create {
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o777 != 0o700 {
        return Err(io::Error::other(
            "service directory must be a real, owned, mode-0700 directory",
        ));
    }
    Ok(())
}

// Caller holds the workspace owner lock. A per-endpoint sibling also keeps fallback
// workspaces independent and stays within the final socket's path-length budget.
fn listen(socket: &Path) -> io::Result<UnixListener> {
    let pending = socket.with_extension("tmp");
    for path in [socket, &pending] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == uid() => {
                fs::remove_file(path)?;
            }
            Ok(_) => {
                return Err(io::Error::other(
                    "refusing to remove a non-socket or unowned service endpoint",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    let listener = UnixListener::bind(&pending)?;
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    // Clients see absence until the fully permissioned socket is atomically published.
    // Do not change the process-wide umask: serve() can run in a multithreaded host.
    fs::rename(&pending, socket)?;
    Ok(listener)
}

/// Runs one foreground Linux/macOS service for a canonical workspace root until terminated.
///
/// An OS file lock at the root protects ownership, including when a long socket path uses
/// the private per-user `/tmp` fallback. Competing owners return `WouldBlock` before indexing.
/// `STACKSCOUT_INDEX_THREADS` selects 1–8 indexing threads (default: available CPUs, capped at 4).
/// `STACKSCOUT_WATCH=0` disables automatic watching/reconciliation for explicit-update workflows.
pub fn serve(root: &Path) -> io::Result<()> {
    let (root, socket) = paths(root)?;
    let directory = root.join(".stackscout");
    private_directory(&directory, true)?;
    private_directory(socket.parent().expect("socket has a parent"), true)?;
    let owner = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("owner.lock"))?;
    let metadata = owner.metadata()?;
    if !metadata.is_file() || metadata.uid() != uid() || metadata.mode() & 0o777 != 0o600 {
        return Err(io::Error::other(
            "service lock must be an owned, mode-0600 regular file",
        ));
    }
    owner.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "service already owned; reconnect",
        ),
        fs::TryLockError::Error(error) => error,
    })?;
    let threads = match std::env::var("STACKSCOUT_INDEX_THREADS") {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=8).contains(value))
            .ok_or_else(|| io::Error::other("STACKSCOUT_INDEX_THREADS must be 1–8"))?,
        Err(std::env::VarError::NotPresent) => thread::available_parallelism()
            .map_or(1, usize::from)
            .min(4),
        Err(error) => return Err(io::Error::other(error)),
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .map_err(io::Error::other)?;
    let watch_enabled = std::env::var("STACKSCOUT_WATCH").as_deref() != Ok("0");
    let listener = listen(&socket)?;
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
        watch: Arc::new(Mutex::new(super::watch::Pending::default())),
    };
    let repo = crate::Repository::new().map_err(io::Error::other)?;
    let (updates, receiver) = mpsc::sync_channel(8);
    let (queries, readers) = mpsc::sync_channel::<(UnixStream, protocol::Operation)>(8);
    let readers = Mutex::new(readers);
    thread::scope(|scope| {
        scope.spawn(|| super::worker(&shared, repo, receiver, pool, watch_enabled));
        for _ in 0..4 {
            let shared = &shared;
            let readers = &readers;
            scope.spawn(move || {
                loop {
                    let job = readers.lock().expect("query queue poisoned").recv();
                    let Ok((stream, operation)) = job else { break };
                    let _ = reply(stream, &super::query(shared, operation));
                }
            });
        }
        let result = ingress(&listener, &shared, &updates, &queries);
        drop(updates);
        drop(queries);
        result
    })
}

struct Client {
    token: Token,
    stream: UnixStream,
    bytes: Vec<u8>,
    deadline: Instant,
}

impl Client {
    fn unregister(&self, registry: &mio::Registry) -> io::Result<()> {
        registry.deregister(&mut SourceFd(&self.stream.as_raw_fd()))
    }

    // Drain readiness, bounded by one maximum-size frame and its absolute deadline.
    fn read(&mut self) -> io::Result<Option<usize>> {
        let mut buffer = [0; 4096];
        loop {
            if Instant::now() >= self.deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "request deadline exceeded",
                ));
            }
            match self.stream.read(&mut buffer) {
                Ok(0) => return Err(io::Error::other("request must end with a newline")),
                Ok(count) => {
                    self.bytes.extend_from_slice(&buffer[..count]);
                    if self.bytes.len() as u64 > protocol::MAX_REQUEST {
                        return Err(io::Error::other("request exceeds 64 KiB"));
                    }
                    if let Some(end) = buffer[..count].iter().position(|byte| *byte == b'\n') {
                        return Ok(Some(self.bytes.len() - count + end));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

// Readiness-driven ingress; slow senders never occupy graph-query workers.
fn ingress(
    listener: &UnixListener,
    shared: &Shared,
    updates: &mpsc::SyncSender<Update>,
    queries: &mpsc::SyncSender<(UnixStream, protocol::Operation)>,
) -> io::Result<()> {
    let mut poll = Poll::new()?;
    poll.registry().register(
        &mut SourceFd(&listener.as_raw_fd()),
        Token(0),
        Interest::READABLE,
    )?;
    let mut events = Events::with_capacity(65);
    let mut clients = VecDeque::<Client>::new();
    let mut next = 1usize;
    let mut accepting = false;
    loop {
        let mut timeout = clients
            .front()
            .map(|client| client.deadline.saturating_duration_since(Instant::now()));
        if accepting {
            // An edge-triggered listener may still have backlog after the acceptance budget.
            timeout = Some(Duration::ZERO);
        }
        match poll.poll(&mut events, timeout) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        while clients
            .front()
            .is_some_and(|client| Instant::now() >= client.deadline)
        {
            clients.pop_front().unwrap().unregister(poll.registry())?;
        }
        for event in &events {
            if event.token() == Token(0) {
                accepting = true;
                continue;
            }
            let Some(index) = clients
                .iter()
                .position(|client| client.token == event.token())
            else {
                continue;
            };
            let result = clients[index].read();
            if matches!(result, Ok(None)) {
                continue;
            }
            let client = clients.remove(index).unwrap();
            client.unregister(poll.registry())?;
            match result {
                Ok(Some(end)) => dispatch(
                    client.stream,
                    &client.bytes[..end],
                    shared,
                    updates,
                    queries,
                ),
                Err(error) => reject(client.stream, &shared.failure(error.to_string())),
                Ok(None) => unreachable!("incomplete clients remain registered"),
            }
        }
        if !accepting {
            continue;
        }
        for _ in 0..32 {
            match listener.accept() {
                Ok((stream, _)) => {
                    if check_peer(&stream, uid()).is_err() {
                        continue; // Fail closed, without responding or admitting a client frame.
                    }
                    stream.set_nonblocking(true)?;
                    if clients.len() == 64 {
                        clients.pop_front().unwrap().unregister(poll.registry())?;
                    }
                    let token = Token(next);
                    next = next
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("client token exhausted"))?;
                    poll.registry().register(
                        &mut SourceFd(&stream.as_raw_fd()),
                        token,
                        Interest::READABLE,
                    )?;
                    clients.push_back(Client {
                        token,
                        stream,
                        bytes: Vec::new(),
                        deadline: Instant::now() + Duration::from_secs(5),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    accepting = false;
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

fn dispatch(
    stream: UnixStream,
    bytes: &[u8],
    shared: &Shared,
    updates: &mpsc::SyncSender<Update>,
    queries: &mpsc::SyncSender<(UnixStream, protocol::Operation)>,
) {
    let request: protocol::Request = match serde_json::from_slice(bytes) {
        Ok(request) => request,
        Err(error) => return reject(stream, &shared.failure(format!("invalid request: {error}"))),
    };
    if request.version != protocol::VERSION {
        return reject(
            stream,
            &shared.failure(format!("unsupported protocol version: {}", request.version)),
        );
    }
    let paths = match request.operation {
        protocol::Operation::Sync { paths } => Some(paths),
        protocol::Operation::Rescan {} => None,
        operation => {
            if let Err(
                mpsc::TrySendError::Full((stream, _))
                | mpsc::TrySendError::Disconnected((stream, _)),
            ) = queries.try_send((stream, operation))
            {
                reject(
                    stream,
                    &shared.failure("query queue unavailable; retry the request"),
                );
            }
            return;
        }
    };
    if let Err(mpsc::TrySendError::Full(update) | mpsc::TrySendError::Disconnected(update)) =
        updates.try_send(Update {
            paths,
            reply: stream,
        })
    {
        reject(
            update.reply,
            &shared.failure("update queue unavailable; retry the request"),
        );
    }
}

fn reject(mut stream: UnixStream, response: &protocol::Response) {
    // Ingress must not wait for a client to read an error. A partial error is a transport failure.
    if let Ok(mut bytes) = serde_json::to_vec(response) {
        bytes.push(b'\n');
        let _ = stream.write(&bytes);
    }
}

pub(super) fn reply(mut stream: UnixStream, response: &protocol::Response) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(response)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > protocol::MAX_RESPONSE {
        return Err(io::Error::other("response exceeds 1 MiB"));
    }
    stream.set_nonblocking(false)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut remaining = bytes.as_slice();
    while !remaining.is_empty() {
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "response deadline exceeded"))?;
        stream.set_write_timeout(Some(timeout))?;
        let count = stream.write(remaining)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "client stopped reading",
            ));
        }
        remaining = &remaining[count..];
    }
    Ok(())
}

/// Returns the canonical root and permission-checked Unix socket path.
///
/// Does not connect or start an owner; an owned stale socket can still pass these checks.
/// Revalidate before connecting, and validate the response's root and protocol identity.
///
/// ```
/// let root = tempfile::tempdir()?;
/// assert!(stackscout::service::endpoint(root.path()).is_err());
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn endpoint(root: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let (root, socket) = paths(root)?;
    private_directory(&root.join(".stackscout"), false)?;
    private_directory(socket.parent().expect("socket has a parent"), false)?;
    let metadata = fs::symlink_metadata(&socket)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != uid()
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(io::Error::other(
            "service endpoint must be an owned, mode-0600 socket",
        ));
    }
    Ok((root, socket))
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
    let mut stream = UnixStream::connect(socket)?;
    check_peer(&stream, uid())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connected_peers_require_the_expected_uid() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("service.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let client = UnixStream::connect(&socket).unwrap();
        let (server, _) = listener.accept().unwrap();
        for stream in [&client, &server] {
            check_peer(stream, uid()).unwrap();
            assert_eq!(
                check_peer(stream, uid().wrapping_add(1))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied,
            );
        }
    }

    #[test]
    fn publication_recovers_staged_sockets_and_accepts_on_the_final_path() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("service.sock");
        let pending = socket.with_extension("tmp");
        // Simulate crashes both before and after publication; a different root's sibling
        // must not be removed, even when fallback endpoints share one directory.
        drop(UnixListener::bind(&socket).unwrap());
        drop(UnixListener::bind(&pending).unwrap());
        fs::set_permissions(&pending, fs::Permissions::from_mode(0o777)).unwrap();
        let other = directory.path().join("other.tmp");
        fs::write(&other, "keep").unwrap();
        let listener = listen(&socket).unwrap();
        assert!(!pending.exists());
        let metadata = fs::symlink_metadata(&socket).unwrap();
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), uid());
        assert_eq!(fs::read_to_string(other).unwrap(), "keep");
        let _client = UnixStream::connect(&socket).unwrap();
        assert!(listener.accept().is_ok());
    }

    #[test]
    fn publication_refuses_non_sockets_and_symlinks_at_either_path() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("service.sock");
        let target = directory.path().join("keep");
        fs::write(&target, "keep").unwrap();
        for path in [socket.clone(), socket.with_extension("tmp")] {
            fs::write(&path, "keep").unwrap();
            assert!(listen(&socket).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(listen(&socket).is_err());
            assert!(
                fs::symlink_metadata(&path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
            fs::remove_file(&path).unwrap();
            if uid() == 0 {
                let listener = UnixListener::bind(&path).unwrap();
                std::os::unix::fs::chown(&path, Some(1), None).unwrap();
                assert!(listen(&socket).is_err());
                assert_eq!(fs::symlink_metadata(&path).unwrap().uid(), 1);
                drop(listener);
                fs::remove_file(&path).unwrap();
            }
        }
    }
}
