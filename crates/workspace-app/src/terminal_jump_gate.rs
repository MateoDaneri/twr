//! One in-flight terminal jump, retaining its owner until a backend reply.
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
    valid: bool,
    started: Instant,
    warned: bool,
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

pub struct Gate<T> {
    next: Option<Token>,
    pending: Option<Pending<T>>,
}

impl<T> Default for Gate<T> {
    fn default() -> Self {
        Self {
            next: Token::new(1),
            pending: None,
        }
    }
}

impl<T> Gate<T> {
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Inspect the owner without releasing the reservation or accepting a
    /// backend reply. The caller must still match the reply's token.
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
        if self.pending.is_some() {
            return Err(payload);
        }
        let Some(token) = self.next else {
            return Err(payload);
        };
        self.next = token.get().checked_add(1).and_then(Token::new);
        self.pending = Some(Pending {
            token,
            context,
            valid: true,
            started: Instant::now(),
            warned: false,
            payload,
        });
        Ok(token)
    }

    /// The enqueue callback must return `Err` only when the backend did not
    /// accept the command. Rejected reservations consume their token but do
    /// not leave the gate stuck; a panic retains it conservatively.
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

    /// Use for any input, focus change, timeout or changed source guard. Keep
    /// the in-flight token until the matching reply actually arrives.
    pub fn invalidate(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.valid = false;
        }
    }

    /// Timeouts do not free the gate: the command may still have executed.
    pub fn expire(&mut self, now: Instant, limit: Duration) -> bool {
        let Some(pending) = &mut self.pending else {
            return false;
        };
        if pending.warned || now.saturating_duration_since(pending.started) < limit {
            return false;
        }
        pending.warned = true;
        pending.valid = false;
        true
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
            RestoreCopyMark::Restored if pending.valid && current => {
                Completion::Ready(pending.payload)
            }
            RestoreCopyMark::Restored => Completion::StaleAfterMove(pending.payload),
            RestoreCopyMark::RejectedBeforeMove => Completion::Rejected(pending.payload),
            RestoreCopyMark::UncertainAfterMove => Completion::Uncertain(pending.payload),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context {
        Context {
            workspace: 1,
            pane: 2,
            attachment: 3,
        }
    }

    #[test]
    fn deferred_jump_requires_original_position_and_normal_mode() {
        use workspace_core::editor_jumplist::{Location, View};
        let origin = Location {
            buffer: 8,
            row: 3,
            column: 4,
            view: View::default(),
        };
        let jump = DeferredJump::new(origin, true, 3);
        assert_eq!(jump.take_if_current(Some(origin), true), Some((true, 3)));
        assert_eq!(jump.take_if_current(Some(origin), false), None);
        assert_eq!(jump.take_if_current(None, true), None);
        assert_eq!(
            jump.take_if_current(
                Some(Location {
                    column: 5,
                    ..origin
                }),
                true
            ),
            None
        );
        assert_eq!(
            jump.take_if_current(
                Some(Location {
                    buffer: 9,
                    ..origin
                }),
                true
            ),
            None
        );
    }

    #[test]
    fn read_capture_retains_first_result_only_for_matching_path() {
        use std::path::{Path, PathBuf};
        let path = PathBuf::from("/synthetic/a.txt");
        let mut first_mark = PendingFileCapture::new(path.clone());
        assert!(first_mark.receive(Ok::<_, &str>(12)).is_ok());
        assert!(first_mark.receive(Ok(13)).is_err());
        assert_eq!(first_mark.take(Path::new("/synthetic/b.txt")), None);
        assert_eq!(first_mark.take(&path), Some(Ok(12)));
        assert_eq!(first_mark.take(&path), None);
        assert!(first_mark.receive(Ok(14)).is_err());
        let mut first_read = PendingFileCapture::new(path.clone());
        assert_eq!(first_read.take(&path), None);
        assert!(first_read.receive(Err::<u8, _>("capture failed")).is_ok());
        assert_eq!(first_read.take(&path), Some(Err("capture failed")));
    }

    #[test]
    fn context_and_token_must_both_match_before_committing() {
        let mut gate = Gate::default();
        let token = gate.begin(context(), "jump").unwrap();
        assert!(gate.begin(context(), "second").is_err());
        assert!(
            gate.finish(
                token,
                Context {
                    pane: 9,
                    ..context()
                },
                RestoreCopyMark::Restored,
                true
            )
            .is_none()
        );
        assert!(gate.busy());
        assert!(matches!(
            gate.finish(token, context(), RestoreCopyMark::Restored, true),
            Some(Completion::Ready("jump"))
        ));
        assert!(!gate.busy());
        let next = gate.begin(context(), "next").unwrap();
        assert_ne!(token, next);
        assert!(
            gate.finish(token, context(), RestoreCopyMark::Restored, true)
                .is_none()
        );
        assert!(gate.busy());
    }

    #[test]
    fn focus_round_trip_or_timeout_never_commits_a_late_success() {
        let mut gate = Gate::default();
        let token = gate.begin(context(), 7).unwrap();
        gate.invalidate();
        assert!(gate.begin(context(), 8).is_err());
        assert!(matches!(
            gate.finish(token, context(), RestoreCopyMark::Restored, true),
            Some(Completion::StaleAfterMove(7))
        ));
        let next = gate.begin(context(), 9).unwrap();
        assert!(matches!(
            gate.finish(next, context(), RestoreCopyMark::Restored, false),
            Some(Completion::StaleAfterMove(9))
        ));
    }

    #[test]
    fn timeout_warns_once_and_keeps_token_until_reply() {
        let mut gate = Gate::default();
        let token = gate.begin(context(), 7).unwrap();
        let future = Instant::now() + Duration::from_secs(16);
        assert!(gate.expire(future, Duration::from_secs(15)));
        assert!(!gate.expire(future, Duration::from_secs(15)));
        assert!(gate.begin(context(), 8).is_err());
        assert!(matches!(
            gate.finish(token, context(), RestoreCopyMark::Restored, true),
            Some(Completion::StaleAfterMove(7))
        ));
    }

    #[test]
    fn backend_rejection_and_uncertain_execution_are_not_confused() {
        let mut gate = Gate::default();
        let first = gate.begin(context(), "first").unwrap();
        gate.invalidate();
        assert!(matches!(
            gate.finish(first, context(), RestoreCopyMark::RejectedBeforeMove, false),
            Some(Completion::Rejected("first"))
        ));
        let second = gate.begin(context(), "second").unwrap();
        assert!(matches!(
            gate.finish(second, context(), RestoreCopyMark::UncertainAfterMove, true),
            Some(Completion::Uncertain("second"))
        ));
    }

    #[test]
    fn tokens_do_not_wrap_and_reuse_old_replies() {
        let mut gate = Gate {
            next: Token::new(u64::MAX),
            ..Gate::default()
        };
        let token = gate.begin(context(), 1).unwrap();
        assert!(matches!(
            gate.finish(token, context(), RestoreCopyMark::RejectedBeforeMove, true),
            Some(Completion::Rejected(1))
        ));
        assert_eq!(gate.begin(context(), 2), Err(2));
    }

    #[test]
    fn refused_enqueue_releases_payload_without_reusing_the_token() {
        let mut gate = Gate::default();
        let mut rejected = None;
        assert_eq!(
            gate.try_dispatch(context(), "payload", |token| {
                rejected = Some(token);
                Err(())
            }),
            Err("payload")
        );
        assert!(!gate.busy());
        let accepted = gate.try_dispatch(context(), "next", |_| Ok(())).unwrap();
        assert_ne!(Some(accepted), rejected);
        assert!(
            gate.finish(
                rejected.unwrap(),
                context(),
                RestoreCopyMark::Restored,
                true
            )
            .is_none()
        );
        assert!(gate.busy());
    }
}
