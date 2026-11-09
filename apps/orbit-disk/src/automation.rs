//! The automation socket. The transport serves each request on its own thread; the handler
//! hops to the UI thread, runs `Controller::handle_bytes` against the `AppState` there, performs
//! `take_automation_effects()` (via `AppHost::automation`) and hands the response back. A request
//! cancelled by the transport (peer gone, deadline, shutdown) is skipped or its answer dropped.

use std::path::Path;
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

use futures::StreamExt;
use gpui::App;
use orbit_app_state::AppState;
use orbit_automation::Controller;
use orbit_automation::transport::{Cancellation, Server, endpoint};
use orbit_ui::Host;

struct Job {
    request: Vec<u8>,
    reply: SyncSender<Vec<u8>>,
    cancel: Cancellation,
}

/// How often a waiting handler re-checks its cancellation.
const POLL: Duration = Duration::from_millis(50);

/// Starts the server at `endpoint(runtime)`. Returns it so the app can stop it when quitting.
pub fn start(host: &Host, runtime: &Path, cx: &mut App) -> Option<Server> {
    let (sender, mut jobs) = futures::channel::mpsc::unbounded::<Job>();
    let server = Server::new(endpoint(runtime), move |request: Vec<u8>, cancel: &Cancellation| {
        if cancel.is_cancelled() {
            return Vec::new();
        }
        let (reply, response) = sync_channel(1);
        if sender.unbounded_send(Job { request, reply, cancel: cancel.clone() }).is_err() {
            return Vec::new();
        }
        loop {
            match response.recv_timeout(POLL) {
                Ok(bytes) => return bytes,
                Err(RecvTimeoutError::Timeout) if !cancel.is_cancelled() => {}
                Err(_) => return Vec::new(),
            }
        }
    });
    let host = host.clone();
    cx.spawn(async move |cx| {
        let mut controller = Controller::<AppState>::default();
        while let Some(job) = jobs.next().await {
            if job.cancel.is_cancelled() {
                continue;
            }
            let response = cx.update(|cx| {
                host.update(cx, |host, cx| host.automation(|state| controller.handle_bytes(state, &job.request), cx))
            });
            match response {
                Ok(bytes) => {
                    let _ = job.reply.send(bytes);
                }
                Err(_) => break,
            }
        }
    })
    .detach();
    match server.start() {
        Ok(()) => Some(server),
        Err(error) => {
            eprintln!("orbit-disk: automation is unavailable: {error}");
            None
        }
    }
}
