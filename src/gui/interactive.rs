//! GUI implementation of [`InteractivePolicy`].
//!
//! Bridges the worker thread (where `Installer::run()` lives) and the
//! main thread (where egui renders) using a single mpsc queue and a
//! per-request `sync_channel(1)` reply slot.
//!
//! The worker thread:
//!   1. Builds a [`GuiPromptRequest`] containing the [`PacmanInvocation`]
//!      and a `Sender` for the reply.
//!   2. Sends the request through the prompt queue.
//!   3. Blocks on the matching `Receiver` until the main thread sends a
//!      [`PacmanDecision`] back.
//!
//! The main thread:
//!   1. Drains the prompt queue each frame.
//!   2. When a request lands, opens a modal.
//!   3. On a button click, sends the chosen decision through the reply
//!      channel and closes the modal.

use crate::utils::interactive::{
    ExtraPackages, InteractivePolicy, PacmanDecision, PacmanInvocation,
};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Mutex;

/// A request from the worker to the main thread.
pub enum GuiPromptRequest {
    ConfirmPacman {
        inv: PacmanInvocation,
        reply: SyncSender<PacmanDecision>,
    },
    PromptExtras {
        can_use_yay: bool,
        reply: SyncSender<(ExtraPackages, bool)>,
    },
}

/// Policy used by the GUI worker thread.  `tx` is the queue end the
/// main thread drains.
pub struct GuiPolicy {
    tx: Mutex<std::sync::mpsc::Sender<GuiPromptRequest>>,
}

impl GuiPolicy {
    pub fn new(tx: std::sync::mpsc::Sender<GuiPromptRequest>) -> Self {
        Self { tx: Mutex::new(tx) }
    }
}

impl InteractivePolicy for GuiPolicy {
    fn confirm_pacman(&self, inv: &PacmanInvocation) -> PacmanDecision {
        let (reply_tx, reply_rx): (SyncSender<PacmanDecision>, Receiver<PacmanDecision>) =
            sync_channel(1);
        let req = GuiPromptRequest::ConfirmPacman {
            inv: inv.clone(),
            reply: reply_tx,
        };
        if let Ok(tx) = self.tx.lock() {
            if tx.send(req).is_err() {
                // Main thread is gone — auto-approve to avoid deadlock.
                return PacmanDecision::Approve;
            }
        }
        // Block until the main thread answers.  If the reply channel is
        // dropped without a send (e.g. window closed mid-prompt) treat
        // it as Cancel — emergency cleanup will run.
        reply_rx.recv().unwrap_or(PacmanDecision::Cancel)
    }

    fn prompt_extras(&self, can_use_yay: bool) -> (ExtraPackages, bool) {
        type ExtrasReply = (ExtraPackages, bool);
        let (reply_tx, reply_rx): (SyncSender<ExtrasReply>, Receiver<ExtrasReply>) =
            sync_channel(1);
        let req = GuiPromptRequest::PromptExtras {
            can_use_yay,
            reply: reply_tx,
        };
        if let Ok(tx) = self.tx.lock() {
            if tx.send(req).is_err() {
                return (ExtraPackages::default(), false);
            }
        }
        reply_rx
            .recv()
            .unwrap_or((ExtraPackages::default(), false))
    }
}
