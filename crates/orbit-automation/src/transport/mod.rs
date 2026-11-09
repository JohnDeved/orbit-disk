//! Unix-domain socket transport between `OrbitControl` and the running app.
//!
//! One request and one response per connection, each framed by a 4-byte big-endian length and
//! capped at [`MAX_FRAME`]. The socket lives in a private 0700 directory, is itself 0600, and
//! both ends verify the peer's user. A lock file keeps one server per user; a stale socket left
//! by a crashed server is replaced only after a failed connection probe, and only if it is still
//! the same inode.

mod endpoint;
#[cfg(test)]
mod tests;
mod wire;

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub use wire::MAX_FRAME;

use self::endpoint::{EndpointDirectory, SocketIdentity};
use self::wire::errno;
use crate::TransportError;

/// File name of the socket inside the runtime directory.
pub const SOCKET_NAME: &str = "control.sock";
/// Concurrent connections served; further connections are closed immediately.
pub const MAX_CONNECTIONS: usize = 8;

/// The socket path inside a runtime directory (`AppDirs::runtime`).
pub fn endpoint(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SOCKET_NAME)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    client: Duration,
    read: Duration,
    handler: Duration,
    write: Duration,
    /// Poll interval while a handler runs, bounding how late a disconnect is noticed.
    probe: Duration,
    expected_uid: libc::uid_t,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            client: Duration::from_secs(45),
            read: Duration::from_secs(10),
            handler: Duration::from_secs(30),
            write: Duration::from_secs(10),
            probe: Duration::from_millis(100),
            expected_uid: wire::effective_uid(),
        }
    }
}

/// Set when a request no longer needs an answer: the peer disconnected, the handler deadline
/// passed, or the server stopped. Handlers should check it before acting and while waiting.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<(Mutex<bool>, Condvar)>);

impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        *lock(&self.0.0) = true;
        self.0.1.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        *lock(&self.0.0)
    }

    /// Blocks up to `timeout`; returns whether the request was cancelled.
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = lock(&self.0.0);
        let (guard, _) =
            self.0.1.wait_timeout_while(guard, timeout, |cancelled| !*cancelled).unwrap_or_else(|p| p.into_inner());
        *guard
    }
}

/// Serves one request. Runs on its own thread; its result is discarded once cancelled.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, request: Vec<u8>, cancellation: &Cancellation) -> Vec<u8>;
}

impl<F> Handler for F
where
    F: Fn(Vec<u8>, &Cancellation) -> Vec<u8> + Send + Sync + 'static,
{
    fn handle(&self, request: Vec<u8>, cancellation: &Cancellation) -> Vec<u8> {
        self(request, cancellation)
    }
}

/// Sends one request to the app and waits up to 45 s for its response.
#[derive(Clone, Debug)]
pub struct Client {
    endpoint: PathBuf,
    limits: Limits,
}

impl Client {
    pub fn new(endpoint: impl Into<PathBuf>) -> Self {
        Self { endpoint: endpoint.into(), limits: Limits::default() }
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// [`TransportError::Unavailable`] means nothing was sent; every other error after the
    /// connection was made leaves the request's outcome unknown.
    pub fn request(&self, payload: &[u8]) -> Result<Vec<u8>, TransportError> {
        if payload.len() > MAX_FRAME {
            return Err(TransportError::OversizedMessage);
        }
        let directory = EndpointDirectory::open(&self.endpoint, false)?;
        if directory.socket_identity()?.is_none() {
            return Err(TransportError::Unavailable);
        }
        drop(directory);
        let fd = wire::socket()?;
        let deadline = Instant::now() + self.limits.client;
        match wire::connect(fd.as_raw_fd(), &self.endpoint, deadline) {
            Err(TransportError::System(_, code)) if code == libc::ENOENT || code == libc::ECONNREFUSED => {
                return Err(TransportError::Unavailable);
            }
            result => result?,
        }
        wire::verify_peer(fd.as_raw_fd(), self.limits.expected_uid)?;
        wire::write_frame(fd.as_raw_fd(), payload, deadline)?;
        wire::read_frame(fd.as_raw_fd(), deadline)
    }
}

/// The app side. [`Server::start`] binds the endpoint and serves on background threads until
/// [`Server::stop`] or drop. Starting twice is a no-op; a stopped server can start again.
pub struct Server {
    endpoint: PathBuf,
    handler: Arc<dyn Handler>,
    limits: Limits,
    run: Mutex<Option<ServerRun>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("endpoint", &self.endpoint).finish_non_exhaustive()
    }
}

impl Server {
    pub fn new(endpoint: impl Into<PathBuf>, handler: impl Handler) -> Self {
        Self { endpoint: endpoint.into(), handler: Arc::new(handler), limits: Limits::default(), run: Mutex::new(None) }
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    pub fn start(&self) -> Result<(), TransportError> {
        let mut run = lock(&self.run);
        if run.is_none() {
            *run = Some(ServerRun::start(&self.endpoint, self.handler.clone(), self.limits)?);
        }
        Ok(())
    }

    /// Closes the listener, disconnects every client and cancels pending handlers without
    /// waiting for them. Removes the socket file if it is still the one this server bound.
    pub fn stop(&self) {
        if let Some(run) = lock(&self.run).take() {
            run.stop();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Connection {
    fd: OwnedFd,
    state: Mutex<ConnectionState>,
}

#[derive(Default)]
struct ConnectionState {
    interrupted: bool,
    cancellation: Option<Cancellation>,
}

impl Connection {
    fn attach(&self, cancellation: &Cancellation) {
        let mut state = lock(&self.state);
        if state.interrupted {
            cancellation.cancel();
        } else {
            state.cancellation = Some(cancellation.clone());
        }
    }

    /// Wakes the serving thread. The descriptor stays open until the last reference drops, so
    /// it cannot be reused while another thread still uses this number.
    fn interrupt(&self) {
        let mut state = lock(&self.state);
        state.interrupted = true;
        if let Some(cancellation) = state.cancellation.take() {
            cancellation.cancel();
        }
        wire::shutdown(self.fd.as_raw_fd());
    }
}

struct Shared {
    handler: Arc<dyn Handler>,
    limits: Limits,
    stopped: AtomicBool,
    connections: Mutex<HashMap<u64, Arc<Connection>>>,
    next_id: AtomicU64,
}

impl Shared {
    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    fn interrupt_all(&self) {
        for connection in lock(&self.connections).values() {
            connection.interrupt();
        }
    }
}

struct ServerRun {
    shared: Arc<Shared>,
    wake: OwnedFd,
    accept: JoinHandle<()>,
}

/// Everything the accept thread owns and releases (in this order) when it exits.
struct Listening {
    listener: OwnedFd,
    wake: OwnedFd,
    identity: SocketIdentity,
    lock_file: OwnedFd,
    directory: EndpointDirectory,
}

impl ServerRun {
    fn start(endpoint: &Path, handler: Arc<dyn Handler>, limits: Limits) -> Result<Self, TransportError> {
        let directory = EndpointDirectory::open(endpoint, true)?;
        let lock_fd = directory.lock()?;
        if let Some(old) = directory.socket_identity()? {
            let probe = wire::socket()?;
            match wire::connect(probe.as_raw_fd(), endpoint, Instant::now() + Duration::from_secs(1)) {
                Ok(()) => return Err(TransportError::AlreadyRunning),
                Err(TransportError::System(_, code)) if code == libc::ECONNREFUSED || code == libc::ENOENT => {
                    directory.unlink_socket(old);
                }
                Err(error) => return Err(error),
            }
        }
        let listener = wire::socket()?;
        wire::bind(listener.as_raw_fd(), endpoint)?;
        let bound = (|| {
            directory.protect_socket()?;
            let identity = directory.socket_identity()?.ok_or(TransportError::Unavailable)?;
            // SAFETY: listen on an owned, bound socket.
            if unsafe { libc::listen(listener.as_raw_fd(), MAX_CONNECTIONS as libc::c_int) } != 0 {
                return Err(TransportError::System("Listen for automation", errno()));
            }
            Ok(identity)
        })();
        let identity = match bound {
            Ok(identity) => identity,
            Err(error) => {
                if let Ok(Some(identity)) = directory.socket_identity() {
                    directory.unlink_socket(identity);
                }
                return Err(error);
            }
        };
        let (wake_read, wake_write) = match pipe() {
            Ok(pair) => pair,
            Err(error) => {
                directory.unlink_socket(identity);
                return Err(error);
            }
        };
        let shared = Arc::new(Shared {
            handler,
            limits,
            stopped: AtomicBool::new(false),
            connections: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        });
        let listening = Listening { listener, wake: wake_read, identity, lock_file: lock_fd, directory };
        let accept_shared = shared.clone();
        let accept = thread::Builder::new()
            .name("orbit-automation-accept".into())
            .spawn(move || accept_loop(&accept_shared, listening))
            .map_err(|error| TransportError::System("Start automation", error.raw_os_error().unwrap_or(0)))?;
        Ok(Self { shared, wake: wake_write, accept })
    }

    fn stop(self) {
        self.shared.stopped.store(true, Ordering::Release);
        // SAFETY: writes one byte from a valid buffer to the owned wake pipe.
        unsafe { libc::write(self.wake.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        self.shared.interrupt_all();
        let _ = self.accept.join();
    }
}

fn pipe() -> Result<(OwnedFd, OwnedFd), TransportError> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` has room for the two descriptors pipe(2) returns.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(TransportError::System("Start automation", errno()));
    }
    // SAFETY: both descriptors were just created and are owned by nothing else.
    let pair = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&pair.0, &pair.1] {
        // SAFETY: fcntl on an owned descriptor.
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    Ok(pair)
}

fn accept_loop(shared: &Arc<Shared>, listening: Listening) {
    while !shared.is_stopped() {
        let mut descriptors = [
            libc::pollfd { fd: listening.listener.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: listening.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        // SAFETY: two valid pollfds; no timeout because stop() writes to the wake pipe.
        let result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
        if result < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            break;
        }
        if shared.is_stopped() || descriptors[1].revents != 0 {
            break;
        }
        if descriptors[0].revents == 0 {
            continue;
        }
        // SAFETY: accept on the owned listener; peer address is not requested.
        let raw = unsafe { libc::accept(listening.listener.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut()) };
        if raw < 0 {
            if matches!(errno(), libc::EAGAIN | libc::EINTR | libc::ECONNABORTED) {
                continue;
            }
            break;
        }
        // SAFETY: `raw` is a freshly accepted descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        if wire::configure(fd.as_raw_fd()).is_err()
            || wire::verify_peer(fd.as_raw_fd(), shared.limits.expected_uid).is_err()
        {
            continue;
        }
        let connection = Arc::new(Connection { fd, state: Mutex::default() });
        let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut connections = lock(&shared.connections);
            if shared.is_stopped() || connections.len() >= MAX_CONNECTIONS {
                continue;
            }
            connections.insert(id, connection.clone());
        }
        let serve_shared = shared.clone();
        let spawned = thread::Builder::new().name("orbit-automation-connection".into()).spawn(move || {
            serve(&serve_shared, &connection);
            lock(&serve_shared.connections).remove(&id);
        });
        if spawned.is_err() {
            lock(&shared.connections).remove(&id);
        }
    }
    shared.stopped.store(true, Ordering::Release);
    let Listening { listener, wake, identity, lock_file, directory } = listening;
    drop(listener);
    drop(wake);
    shared.interrupt_all();
    directory.unlink_socket(identity);
    drop(lock_file);
}

/// Malformed, abandoned, cancelled and timed-out requests get no application response.
fn serve(shared: &Shared, connection: &Connection) {
    let fd = connection.fd.as_raw_fd();
    let Ok(request) = wire::read_frame(fd, Instant::now() + shared.limits.read) else { return };
    if shared.is_stopped() {
        return;
    }
    let cancellation = Cancellation::new();
    connection.attach(&cancellation);
    let (sender, receiver) = mpsc::sync_channel(1);
    let handler = shared.handler.clone();
    let task_cancellation = cancellation.clone();
    let spawned = thread::Builder::new().name("orbit-automation-handler".into()).spawn(move || {
        if task_cancellation.is_cancelled() {
            return;
        }
        let response = handler.handle(request, &task_cancellation);
        if !task_cancellation.is_cancelled() {
            let _ = sender.send(response);
        }
    });
    if spawned.is_err() {
        return;
    }
    let deadline = Instant::now() + shared.limits.handler;
    let response = loop {
        match receiver.recv_timeout(shared.limits.probe) {
            Ok(response) => break response,
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {
                if shared.is_stopped() || Instant::now() >= deadline || wire::peer_closed(fd) {
                    cancellation.cancel();
                    return;
                }
            }
        }
    };
    if shared.is_stopped() || cancellation.is_cancelled() {
        return;
    }
    let _ = wire::write_frame(fd, &response, Instant::now() + shared.limits.write);
}
