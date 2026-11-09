//! Ticket-ordered hand-off to the mailbox (Swift `DiscoveryListingSpool`).
//!
//! Every claimed directory gets a ticket in claim order. A child is claimed only after its
//! parent was claimed, so ticket order puts parents before children. Workers publish to the live
//! inventory immediately; only *durable* batches wait here until every earlier ticket is closed,
//! then enter the mailbox in ticket order. A slow directory delays persistence of later ones but
//! never hides their files from views.

use crate::mailbox::{ListingBatch, Mailbox};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[derive(Default)]
struct Listing {
    batches: VecDeque<ListingBatch>,
    closed: bool,
}

#[derive(Default)]
struct State {
    next: u64,
    listings: HashMap<u64, Listing>,
    /// The mailbox refused a batch (cancelled): stop enqueueing.
    failed: bool,
    peak_buffered: usize,
    buffered: usize,
}

pub(crate) struct Spool {
    mailbox: Arc<Mailbox>,
    state: Mutex<State>,
}

impl Spool {
    pub fn new(mailbox: Arc<Mailbox>) -> Self {
        Self { mailbox, state: Mutex::new(State::default()) }
    }

    pub fn append(&self, ticket: u64, batch: ListingBatch) {
        let mut state = self.state.lock();
        if state.failed {
            return;
        }
        if ticket == state.next && state.listings.get(&ticket).is_none_or(|l| l.batches.is_empty()) {
            // Fast path: the current ticket streams straight through.
            if self.mailbox.enqueue(batch).is_err() {
                state.failed = true;
                state.listings.clear();
            }
            return;
        }
        state.listings.entry(ticket).or_default().batches.push_back(batch);
        state.buffered += 1;
        state.peak_buffered = state.peak_buffered.max(state.buffered);
    }

    pub fn close(&self, ticket: u64) {
        let mut state = self.state.lock();
        if state.failed {
            return;
        }
        state.listings.entry(ticket).or_default().closed = true;
        self.drain(&mut state);
    }

    fn drain(&self, state: &mut State) {
        while !state.failed {
            let next = state.next;
            let Some(listing) = state.listings.get_mut(&next) else { return };
            while let Some(batch) = listing.batches.pop_front() {
                state.buffered -= 1;
                if self.mailbox.enqueue(batch).is_err() {
                    state.failed = true;
                    state.listings.clear();
                    return;
                }
            }
            if !listing.closed {
                return;
            }
            state.listings.remove(&next);
            state.next += 1;
        }
    }

    /// All workers ended: finish the mailbox when every ticket drained, otherwise cancel it.
    pub fn finish(&self, cancelled: bool) {
        let mut state = self.state.lock();
        self.drain(&mut state);
        if cancelled || state.failed {
            state.listings.clear();
            return;
        }
        if state.listings.is_empty() {
            self.mailbox.finish();
        } else {
            state.listings.clear();
            self.mailbox.cancel(true);
        }
    }

    pub fn cancel(&self) {
        let mut state = self.state.lock();
        state.failed = true;
        state.listings.clear();
    }

    /// Most batches ever waiting for an earlier ticket.
    pub fn peak_buffered(&self) -> usize {
        self.state.lock().peak_buffered
    }
}
