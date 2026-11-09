//! Behavior tests driven by `FakeBackend` and explicit time. Each test names the Swift checks it
//! covers in `TRIAGE-COVERAGE.md`.

mod collection;
mod hover;
mod mapping;
mod misc;
mod removal;
mod search;

use std::sync::Arc;
use std::time::Duration;

use crate::backend::{Request, ViewSource};
use crate::event::{Event, MappingEvent};
use crate::ids::{DragId, GestureId};
use crate::state::Config;
use crate::testing::Harness;

pub(crate) const HOME: &[u8] = b"/Users/me";

pub(crate) fn p(path: &str) -> Arc<[u8]> {
    Arc::from(path.as_bytes())
}

/// `/r` with two folders and a file; `/s` a second root.
pub(crate) fn harness() -> Harness {
    let mut config = Config::new(HOME);
    config.id_seed = 7;
    let h = Harness::new(config);
    let b = &h.backend;
    b.add_file(b"/r/big/a.bin", 100);
    b.add_file(b"/r/big/b.bin", 50);
    b.add_file(b"/r/docs/x.txt", 30);
    b.add_file(b"/r/docs/y.txt", 20);
    b.add_file(b"/r/docs/deep/z.txt", 5);
    b.add_file(b"/r/file1", 10);
    b.add_file(b"/s/one", 7);
    b.add_file(b"/s/two/three", 3);
    h
}

pub(crate) fn is_preview_view(request: &Request) -> bool {
    matches!(request, Request::View { request, .. } if request.source == ViewSource::Preview)
}

pub(crate) fn is_saved_view(request: &Request) -> bool {
    matches!(request, Request::View { request, .. } if matches!(request.source, ViewSource::Saved(_)))
}

pub(crate) fn current(h: &Harness) -> String {
    h.state.current_path().map(|p| String::from_utf8_lossy(p).into_owned()).unwrap_or_default()
}

pub(crate) fn selected(h: &Harness) -> Option<String> {
    h.state.selected().map(|s| String::from_utf8_lossy(&s.path).into_owned())
}

pub(crate) fn gesture(h: &mut Harness, id: u64) -> GestureId {
    let gesture = GestureId(id);
    h.send(Event::GestureBegin(gesture));
    gesture
}

pub(crate) fn drag_items(h: &mut Harness, id: u64, paths: &[&str]) -> DragId {
    let drag = DragId(id);
    let paths = paths.iter().map(|path| p(path)).collect();
    h.send(Event::DragBegin { drag, source: crate::event::DragSource::Items(paths), label: "items".into() });
    drag
}

pub(crate) fn status(h: &Harness) -> Option<String> {
    h.state.status().map(str::to_owned)
}

pub(crate) fn error(h: &Harness) -> Option<String> {
    h.state.error().map(str::to_owned)
}

pub(crate) const SHORT: Duration = Duration::from_millis(16);
