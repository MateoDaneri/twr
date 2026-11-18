//! Correlate a terminal jump result with its current destination.
//! A timeout or focus change never proves a tmux movement did not happen.
use std::{
    num::NonZeroU64,
    time::{Duration, Instant},
};

use crate::tmux::RestoreCopyMark;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub workspace: usize,
    pub pane: u64,
    pub attachment: u64,
}

pub type Token = NonZeroU64;

/// Holds the single backend response if it arrives before the corresponding
/// disk read. The owner checks the token, workspace and focus separately.
pub struct PendingFileCapture<T> {
    path: std::path::PathBuf,
    received: bool,
    value: Option<T>,
}

impl<T> PendingFileCapture<T> {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            received: false,
            value: None,
        }
    }

    pub fn matches(&self, path: &std::path::Path) -> bool {
        self.path == path
    }

    pub fn receive(&mut self, value: T) -> Result<(), T> {
        if self.received {
            return Err(value);
        }
        self.received = true;
        self.value = Some(value);
        Ok(())
    }

    pub fn take(&mut self, path: &std::path::Path) -> Option<T> {
        self.matches(path).then(|| self.value.take()).flatten()
    }
}

/// One jump requested while a terminal-origin mark is being captured. A later
/// input cancels it; the caller must additionally verify the capture guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeferredJump {
    origin: workspace_core::editor_jumplist::Location,
    older: bool,
    count: usize,
}

impl DeferredJump {
    pub fn new(
        origin: workspace_core::editor_jumplist::Location,
        older: bool,
        count: usize,
    ) -> Self {
        Self {
            origin,
            older,
            count,
        }
    }

    pub fn take_if_current(
        self,
        origin: Option<workspace_core::editor_jumplist::Location>,
        normal: bool,
    ) -> Option<(bool, usize)> {
        (normal && origin == Some(self.origin)).then_some((self.older, self.count))
    }
}

struct Pending<T> {
    token: Token,
    context: Context,
    started: Instant,
    payload: T,
}

pub enum Completion<T> {
    /// Restore succeeded and the original GUI context is still current.
    Ready(T),
    /// The backend guarantees it did not move the tmux cursor.
    Rejected(T),
    /// Restore succeeded, but the original GUI context changed or timed out.
    StaleAfterMove(T),
    /// The command may have moved the cursor; never retry automatically.
    Uncertain(T),
}

pub struct PendingRestore<T> {
    next: Option<Token>,
    pending: Option<Pending<T>>,
}

impl<T> Default for PendingRestore<T> {
    fn default() -> Self {
        Self {
            next: Token::new(1),
            pending: None,
        }
    }
}

impl<T> PendingRestore<T> {
    /// Inspect the latest operation; callers must match its token.
    pub fn pending_payload(&self) -> Option<&T> {
        self.pending.as_ref().map(|pending| &pending.payload)
    }

    pub fn context(&self, token: Token) -> Option<Context> {
        self.pending
            .as_ref()
            .filter(|p| p.token == token)
            .map(|p| p.context)
    }

    fn begin(&mut self, context: Context, payload: T) -> Result<Token, T> {
        let Some(token) = self.next else {
            return Err(payload);
        };
        self.next = token.get().checked_add(1).and_then(Token::new);
        self.pending = Some(Pending {
            token,
            context,
            started: Instant::now(),
            payload,
        });
        Ok(token)
    }

    /// The enqueue callback must return `Err` only when the backend did not
    /// accept the command. Rejected operations consume their token.
    pub fn try_dispatch(
        &mut self,
        context: Context,
        payload: T,
        enqueue: impl FnOnce(Token) -> Result<(), ()>,
    ) -> Result<Token, T> {
        let token = self.begin(context, payload)?;
        if enqueue(token).is_err() {
            return Err(self.pending.take().expect("reservation held").payload);
        }
        Ok(token)
    }

    /// New activity supersedes the GUI result. Old backend replies are ignored.
    pub fn invalidate(&mut self) {
        self.pending = None;
    }

    /// Release a stale request without blocking subsequent navigation.
    pub fn expire(&mut self, now: Instant, limit: Duration) -> bool {
        if self.pending.as_ref().is_some_and(|pending| {
            now.saturating_duration_since(pending.started) >= limit
        }) {
            self.pending = None;
            true
        } else {
            false
        }
    }

    /// A stale/mismatched reply cannot consume or change a newer reservation.
    pub fn finish(
        &mut self,
        token: Token,
        context: Context,
        result: RestoreCopyMark,
        current: bool,
    ) -> Option<Completion<T>> {
        if self
            .pending
            .as_ref()
            .is_none_or(|p| p.token != token || p.context != context)
        {
            return None;
        }
        let pending = self.pending.take().expect("reservation checked");
        Some(match result {
            RestoreCopyMark::Restored if current => {
                Completion::Ready(pending.payload)
            }
            RestoreCopyMark::Restored => Completion::StaleAfterMove(pending.payload),
            RestoreCopyMark::RejectedBeforeMove => Completion::Rejected(pending.payload),
            RestoreCopyMark::UncertainAfterMove => Completion::Uncertain(pending.payload),
        })
    }
}

