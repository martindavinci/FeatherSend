//! Transfer rows shown in the GUI, with cancellation and resume jobs.

use super::{Shared, UiEvent};
use crate::ext::dto::Purpose;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const PROGRESS_EVERY: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq)]
pub enum TransferState {
    Waiting,
    Running,
    Done,
    Failed(String),
    Cancelled,
    /// Interrupted after the automatic retries; can be resumed.
    Paused,
}

impl TransferState {
    pub fn is_final(&self) -> bool {
        matches!(self, TransferState::Done | TransferState::Failed(_) | TransferState::Cancelled)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransferView {
    pub id: u64,
    pub device: String,
    pub item: String,
    pub incoming: bool,
    pub done: u64,
    pub total: u64,
    pub state: TransferState,
    pub resumable: bool,
}

/// What is needed to restart an interrupted outgoing extension transfer.
#[derive(Clone, Debug)]
pub(crate) struct ResumeJob {
    pub device: String,
    pub files: Vec<super::files::LocalFile>,
    pub purpose: Purpose,
}

struct Slot {
    view: TransferView,
    cancel: CancellationToken,
    last_emit: Instant,
    resume: Option<ResumeJob>,
}

#[derive(Default)]
pub(crate) struct TransferTable {
    next: AtomicU64,
    slots: Mutex<HashMap<u64, Slot>>,
}

impl TransferTable {
    pub fn create(
        &self,
        shared: &Shared,
        device: &str,
        item: &str,
        incoming: bool,
        total: u64,
    ) -> (u64, CancellationToken) {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = CancellationToken::new();
        let view = TransferView {
            id,
            device: device.to_string(),
            item: item.to_string(),
            incoming,
            done: 0,
            total,
            state: TransferState::Waiting,
            resumable: false,
        };
        self.slots.lock().unwrap().insert(
            id,
            Slot {
                view: view.clone(),
                cancel: cancel.clone(),
                last_emit: Instant::now(),
                resume: None,
            },
        );
        shared.emit(UiEvent::Transfer(view));
        (id, cancel)
    }

    /// Sets the transferred byte count; emits at most every 200 ms.
    pub fn progress(&self, shared: &Shared, id: u64, done: u64) {
        let mut slots = self.slots.lock().unwrap();
        let Some(slot) = slots.get_mut(&id) else { return };
        slot.view.done = done.min(slot.view.total.max(done));
        if slot.view.state == TransferState::Waiting {
            slot.view.state = TransferState::Running;
        }
        if slot.last_emit.elapsed() >= PROGRESS_EVERY || done >= slot.view.total {
            slot.last_emit = Instant::now();
            let view = slot.view.clone();
            drop(slots);
            shared.emit(UiEvent::Transfer(view));
        }
    }

    pub fn set_state(&self, shared: &Shared, id: u64, state: TransferState) {
        let mut slots = self.slots.lock().unwrap();
        let Some(slot) = slots.get_mut(&id) else { return };
        if slot.view.state == TransferState::Cancelled && state != TransferState::Cancelled {
            return; // a late failure after a cancel stays "cancelled"
        }
        if state == TransferState::Done {
            slot.view.done = slot.view.total;
        }
        slot.view.resumable = state == TransferState::Paused && slot.resume.is_some();
        slot.view.state = state;
        let view = slot.view.clone();
        drop(slots);
        shared.emit(UiEvent::Transfer(view));
    }

    pub fn set_resume(&self, id: u64, job: ResumeJob) {
        if let Some(slot) = self.slots.lock().unwrap().get_mut(&id) {
            slot.resume = Some(job);
        }
    }

    pub fn take_resume(&self, id: u64) -> Option<ResumeJob> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.get_mut(&id)?;
        if slot.view.state != TransferState::Paused {
            return None;
        }
        slot.cancel = CancellationToken::new();
        slot.resume.clone()
    }

    /// A fresh cancellation token for a row that continues with a new
    /// attempt (the old token was cancelled to stop the old writers).
    pub fn renew_token(&self, id: u64) -> Option<CancellationToken> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.get_mut(&id)?;
        if slot.view.state == TransferState::Cancelled {
            return None;
        }
        slot.cancel = CancellationToken::new();
        Some(slot.cancel.clone())
    }

    pub fn token(&self, id: u64) -> Option<CancellationToken> {
        self.slots.lock().unwrap().get(&id).map(|s| s.cancel.clone())
    }

    pub fn state(&self, id: u64) -> Option<TransferState> {
        self.slots.lock().unwrap().get(&id).map(|s| s.view.state.clone())
    }

    pub fn cancel(&self, shared: &Shared, id: u64) {
        let token = {
            let mut slots = self.slots.lock().unwrap();
            let Some(slot) = slots.get_mut(&id) else { return };
            if slot.view.state.is_final() {
                return;
            }
            slot.resume = None;
            slot.cancel.clone()
        };
        token.cancel();
        self.set_state(shared, id, TransferState::Cancelled);
    }
}
