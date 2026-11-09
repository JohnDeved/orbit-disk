//! Real-socket checks of framing, peer verification, endpoint safety, single-instance locking,
//! connection bounds and handler cancellation. Waits are bounded by socket deadlines or by
//! explicit signals from the handler; nothing sleeps for a fixed time.

use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use super::*;

const PATIENCE: Duration = Duration::from_secs(5);

struct Fixture {
    _dir: tempfile::TempDir,
    runtime: PathBuf,
    endpoint: PathBuf,
}

/// A fresh runtime directory under /tmp (short enough for `sun_path`), not yet created.
fn fixture() -> Fixture {
    let dir = tempfile::Builder::new().prefix("oa").tempdir_in("/tmp").unwrap();
    let runtime = dir.path().join("run");
    let endpoint = super::endpoint(&runtime);
    Fixture { _dir: dir, runtime, endpoint }
}

fn echo(request: Vec<u8>, _: &Cancellation) -> Vec<u8> {
    request
}

fn counting(calls: &Arc<AtomicUsize>) -> impl Handler {
    let calls = calls.clone();
    move |request: Vec<u8>, _: &Cancellation| {
        calls.fetch_add(1, Ordering::SeqCst);
        request
    }
}

fn raw(endpoint: &Path) -> UnixStream {
    let stream = UnixStream::connect(endpoint).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    stream
}

/// The server closed the connection without sending anything.
fn assert_closed_without_response(mut stream: UnixStream) {
    let mut byte = [0u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == ErrorKind::ConnectionReset => {}
        other => panic!("expected the server to close the connection, got {other:?}"),
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn frames_round_trip_up_to_the_size_cap() {
    let fixture = fixture();
    let client = Client::new(&fixture.endpoint);
    assert_eq!(client.request(b"{}"), Err(TransportError::Unavailable));
    let server = Server::new(&fixture.endpoint, echo);
    server.start().unwrap();

    let request = "{\"path\":\"/tmp/é 🪐/\\nfile\",\"value\":9223372036854775807}".as_bytes();
    assert_eq!(client.request(request).unwrap(), request);
    assert_eq!(client.request(b"").unwrap(), b"");
    let maximum = vec![0xa5; MAX_FRAME];
    assert_eq!(client.request(&maximum).unwrap(), maximum);
    assert_eq!(client.request(&vec![0; MAX_FRAME + 1]), Err(TransportError::OversizedMessage));

    // Fragmented header and body are reassembled.
    let mut stream = raw(&fixture.endpoint);
    let body = vec![0x6b; 8193];
    for byte in (body.len() as u32).to_be_bytes() {
        stream.write_all(&[byte]).unwrap();
    }
    for chunk in body.chunks(17) {
        stream.write_all(chunk).unwrap();
    }
    let mut reply = vec![0u8; 4 + body.len()];
    stream.read_exact(&mut reply).unwrap();
    assert_eq!((&reply[..4], &reply[4..]), (&(body.len() as u32).to_be_bytes()[..], &body[..]));
}

#[test]
fn wire_rejects_oversized_and_truncated_frames() {
    let deadline = || Instant::now() + PATIENCE;
    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer.write_all(&0u32.to_be_bytes()).unwrap();
    assert_eq!(wire::read_frame(reader.as_raw_fd(), deadline()).unwrap(), b"");
    // The declared length is checked before anything is allocated or read.
    writer.write_all(&(MAX_FRAME as u32 + 1).to_be_bytes()).unwrap();
    assert_eq!(wire::read_frame(reader.as_raw_fd(), deadline()), Err(TransportError::OversizedMessage));

    writer.write_all(&10u32.to_be_bytes()).unwrap();
    writer.write_all(b"abc").unwrap();
    writer.shutdown(std::net::Shutdown::Write).unwrap();
    assert_eq!(wire::read_frame(reader.as_raw_fd(), deadline()), Err(TransportError::Disconnected));

    let (mut writer, reader) = UnixStream::pair().unwrap();
    writer.write_all(&[0, 0]).unwrap();
    drop(writer);
    assert_eq!(wire::read_frame(reader.as_raw_fd(), deadline()), Err(TransportError::Disconnected));
    assert_eq!(
        wire::write_frame(reader.as_raw_fd(), &vec![0; MAX_FRAME + 1], deadline()),
        Err(TransportError::OversizedMessage)
    );
}

#[test]
fn server_drops_malformed_and_slow_requests_without_calling_the_handler() {
    let fixture = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new(&fixture.endpoint, counting(&calls));
    server.limits.read = Duration::from_millis(200);
    server.start().unwrap();

    let mut oversized = raw(&fixture.endpoint);
    oversized.write_all(&[0x7f, 0xff, 0xff, 0xff]).unwrap();
    assert_closed_without_response(oversized);

    let mut truncated = raw(&fixture.endpoint);
    truncated.write_all(&8u32.to_be_bytes()).unwrap();
    truncated.write_all(b"abc").unwrap();
    truncated.shutdown(std::net::Shutdown::Write).unwrap();
    assert_closed_without_response(truncated);

    // A partial frame cannot hold a connection slot past the whole-frame read deadline.
    let mut slow = raw(&fixture.endpoint);
    slow.write_all(&[0, 0, 0]).unwrap();
    assert_closed_without_response(slow);

    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn peers_of_another_user_are_rejected() {
    let other = wire::effective_uid().wrapping_add(1);
    let (left, _right) = UnixStream::pair().unwrap();
    wire::verify_peer(left.as_raw_fd(), wire::effective_uid()).unwrap();
    assert_eq!(wire::verify_peer(left.as_raw_fd(), other), Err(TransportError::PeerMismatch));

    let fixture = fixture();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut server = Server::new(&fixture.endpoint, counting(&calls));
    server.limits.expected_uid = other;
    server.start().unwrap();
    // The server closes the connection before reading a request.
    let refused = Client::new(&fixture.endpoint).request(b"x");
    assert!(matches!(refused, Err(TransportError::Disconnected | TransportError::System(..))), "{refused:?}");
    server.stop();

    let server = Server::new(&fixture.endpoint, counting(&calls));
    server.start().unwrap();
    let mut client = Client::new(&fixture.endpoint);
    client.limits.expected_uid = other;
    // The client refuses to send to a server of another user.
    assert_eq!(client.request(b"x"), Err(TransportError::PeerMismatch));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn one_server_per_endpoint_and_stale_sockets_are_replaced() {
    let fixture = fixture();
    let client = Client::new(&fixture.endpoint);
    let server = Server::new(&fixture.endpoint, echo);
    server.start().unwrap();
    server.start().unwrap();
    let second = Server::new(&fixture.endpoint, echo);
    assert_eq!(second.start(), Err(TransportError::AlreadyRunning));
    assert_eq!(client.request(b"live").unwrap(), b"live");

    server.stop();
    assert!(!fixture.endpoint.exists(), "stop removes the socket it bound");

    // A socket left by a crashed server (nothing listening) is replaced.
    drop(UnixListener::bind(&fixture.endpoint).unwrap());
    std::fs::set_permissions(&fixture.endpoint, PermissionsExt::from_mode(0o600)).unwrap();
    server.start().unwrap();
    assert_eq!(client.request(b"after").unwrap(), b"after");
    server.stop();

    // A live listener that is not ours is never unlinked.
    let foreign = UnixListener::bind(&fixture.endpoint).unwrap();
    std::fs::set_permissions(&fixture.endpoint, PermissionsExt::from_mode(0o600)).unwrap();
    assert_eq!(server.start(), Err(TransportError::AlreadyRunning));
    assert!(fixture.endpoint.exists());
    drop(foreign);
}

#[test]
fn endpoint_is_private_and_never_followed_through_symlinks() {
    let fixture = fixture();
    assert_eq!(Client::new(&fixture.endpoint).request(b"x"), Err(TransportError::Unavailable));
    let server = Server::new(&fixture.endpoint, echo);
    server.start().unwrap();
    assert_eq!(mode(&fixture.runtime), 0o700);
    assert_eq!(mode(&fixture.endpoint), 0o600);
    let lock = fixture.runtime.join("control.lock");
    assert_eq!(mode(&lock), 0o600);
    server.stop();

    let invalid = |server: &Server| matches!(server.start(), Err(TransportError::InvalidEndpoint(_)));
    let target = fixture.runtime.join("target");
    std::fs::write(&target, b"keep").unwrap();
    symlink(&target, &fixture.endpoint).unwrap();
    assert!(invalid(&server), "socket symlink");
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    std::fs::remove_file(&fixture.endpoint).unwrap();

    std::fs::remove_file(&lock).unwrap();
    symlink(&target, &lock).unwrap();
    assert!(invalid(&server), "lock symlink");
    std::fs::remove_file(&lock).unwrap();

    std::fs::set_permissions(&fixture.runtime, PermissionsExt::from_mode(0o755)).unwrap();
    assert!(invalid(&server), "group-readable directory");
    assert!(matches!(Client::new(&fixture.endpoint).request(b"x"), Err(TransportError::InvalidEndpoint(_))));
    std::fs::set_permissions(&fixture.runtime, PermissionsExt::from_mode(0o700)).unwrap();

    let alias = fixture.runtime.join("alias");
    symlink(&fixture.runtime, &alias).unwrap();
    assert!(invalid(&Server::new(super::endpoint(&alias), echo)), "directory symlink");
    server.start().unwrap();
}

#[test]
fn concurrent_connections_are_bounded() {
    let fixture = fixture();
    let server = Server::new(&fixture.endpoint, echo);
    server.start().unwrap();
    let registered = || lock(&server.run).as_ref().map_or(0, |run| lock(&run.shared.connections).len());
    let wait_for = |count: usize| {
        let deadline = Instant::now() + PATIENCE;
        while registered() != count {
            assert!(Instant::now() < deadline, "expected {count} connections, have {}", registered());
            thread::yield_now();
        }
    };
    let idle: Vec<UnixStream> = (0..MAX_CONNECTIONS).map(|_| raw(&fixture.endpoint)).collect();
    wait_for(MAX_CONNECTIONS);
    assert_closed_without_response(raw(&fixture.endpoint));
    drop(idle);
    wait_for(0);
    assert_eq!(Client::new(&fixture.endpoint).request(b"free").unwrap(), b"free");
}

#[test]
fn handler_is_cancelled_when_the_peer_disconnects() {
    let fixture = fixture();
    let (entered_tx, entered) = mpsc::channel();
    let (outcome_tx, outcome) = mpsc::channel();
    let server = Server::new(&fixture.endpoint, move |_: Vec<u8>, cancellation: &Cancellation| {
        entered_tx.send(()).unwrap();
        outcome_tx.send(cancellation.wait(PATIENCE)).unwrap();
        b"late".to_vec()
    });
    server.start().unwrap();
    let mut stream = raw(&fixture.endpoint);
    stream.write_all(&1u32.to_be_bytes()).unwrap();
    stream.write_all(b"x").unwrap();
    entered.recv_timeout(PATIENCE).unwrap();
    drop(stream);
    assert!(outcome.recv_timeout(PATIENCE * 2).unwrap(), "handler saw cancellation");
}

#[test]
fn stop_cancels_pending_handlers_without_waiting_and_restarts_immediately() {
    let fixture = fixture();
    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release) = mpsc::channel::<()>();
    let release = std::sync::Mutex::new(release);
    let finished = Arc::new(AtomicBool::new(false));
    let handler_finished = finished.clone();
    let pending = Server::new(&fixture.endpoint, move |_: Vec<u8>, _: &Cancellation| {
        entered_tx.send(()).unwrap();
        // Ignores cancellation on purpose: stop must not depend on handler cooperation.
        let _ = lock(&release).recv_timeout(PATIENCE * 2);
        handler_finished.store(true, Ordering::SeqCst);
        b"late".to_vec()
    });
    pending.start().unwrap();
    let endpoint = fixture.endpoint.clone();
    let client = thread::spawn(move || Client::new(endpoint).request(b"x"));
    entered.recv_timeout(PATIENCE).unwrap();
    pending.stop();
    assert!(client.join().unwrap().is_err(), "a cancelled handler never answers");
    assert!(!finished.load(Ordering::SeqCst), "stop returned while the handler was still running");

    let server = Server::new(&fixture.endpoint, echo);
    server.start().unwrap();
    assert_eq!(Client::new(&fixture.endpoint).request(b"again").unwrap(), b"again");
    release_tx.send(()).unwrap();
}
