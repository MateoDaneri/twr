//! FIFO resume submissions with acknowledgement-bound recovery, independent of focus.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
const LIMIT: usize = 4096;
const TIMEOUT: Duration = Duration::from_secs(15);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Context {
    pub project: usize,
    pub attachment: u64,
}
#[derive(Clone, PartialEq, Eq)]
struct Batch {
    bytes: Option<Arc<Vec<u8>>>,
    since: Instant,
}
#[derive(Clone, PartialEq, Eq)]
struct Pending {
    token: u64,
    context: Context,
    waiting_ready: bool,
    bytes: Vec<u8>,
    paste_ranges: Vec<(usize, usize)>,
    since: Instant,
    retain_after_enqueue_failure: bool,
    halted_after_backend_failure: bool,
    timed_out: bool,
    rejected_waiting_ack: bool,
    batches: VecDeque<Batch>,
    failed_batches: Vec<Batch>,
    raw_prefix: Option<Arc<Vec<u8>>>,
}
impl Pending {
    fn has_input(&self) -> bool {
        !self.bytes.is_empty() || !self.batches.is_empty()
            || !self.failed_batches.is_empty() || self.raw_prefix.is_some()
    }
    fn awaiting_ack(&self) -> bool {
        self.waiting_ready || !self.batches.is_empty()
    }
}
/// Opaque snapshot used to avoid discarding newer input after a quit prompt.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingGuard {
    pending: Pending,
    uncertain: OnceLock<Option<Arc<Vec<u8>>>>,
}
#[derive(Default)]
pub struct Fence {
    sequence: u64,
    pending: Option<Pending>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredAction {
    Reconnect,
    CloseWorkspace,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DeliveryAck {
    Unknown,
    Pending,
    Finished(Option<DeferredAction>),
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeTimeout {
    NotExpired,
    NoInput,
    Retained,
}

/// Remembers a close request only while the user is not interacting.
#[derive(Default)]
pub struct QuitIntent {
    activity: u64,
    waiting: Option<u64>,
}
impl QuitIntent {
    pub fn request(&mut self) {
        self.waiting = Some(self.activity);
    }
    pub fn activity(&mut self) {
        self.activity = self.activity.wrapping_add(1);
        self.waiting = None;
    }
    pub fn cancel(&mut self) {
        self.activity = self.activity.wrapping_add(1);
        self.waiting = None;
    }
    pub fn take_ready(&mut self, busy: bool) -> Option<u64> {
        if busy { None } else { self.waiting.take() }
    }
    pub fn is_current(&self, generation: u64) -> bool {
        self.activity == generation
    }
}

struct Delivery {
    context: Context,
    waiting_ready: bool,
    batches: VecDeque<Batch>,
    since: Instant,
}

struct FailedDelivery {
    id: u64,
    context: Context,
    bytes: Option<Arc<Vec<u8>>>,
}

/// An opaque, generation-bound reference to one possibly executed batch.
#[derive(Clone, Copy)]
pub struct FailedDeliveryGuard {
    id: u64,
    generation: u64,
}

/// Track acknowledged backend writes separately from keyboard focus. A session
/// must not discard its event receiver until all of its writes are resolved.
#[derive(Default)]
pub struct Deliveries {
    pending: HashMap<(usize, u64), Delivery>,
    failed: VecDeque<FailedDelivery>,
    next_failure: u64,
    deferred: HashMap<usize, DeferredAction>,
    warned: HashSet<usize>,
    generation: u64,
}
/// A wiped terminal identity must not invalidate a pending input destination.
pub fn terminal_delete_busy(
    fence: &Fence,
    deliveries: &Deliveries,
    project: usize,
    search_pending: bool,
    rebind_pending: bool,
    open_pending: bool,
) -> bool {
    fence.active() || deliveries.busy(project) || search_pending || rebind_pending || open_pending
}



/// Transfer outstanding acknowledgements independently of keyboard focus.
/// Input buffered before attachment is enqueued before ownership is moved.
pub fn detach_with_delivery_typed(
    fence: &mut Fence,
    deliveries: &mut Deliveries,
    now: Instant,
    send: impl FnOnce(Context, u64, &[u8], &[(usize, usize)]) -> Result<(), String>,
) -> Result<bool, String> {
    let Some(pending) = fence.pending.as_ref() else {
        return Ok(false);
    };
    if pending.halted_after_backend_failure || pending.timed_out || pending.rejected_waiting_ack {
        return Err(
            "Entrada conservada: copiá o descartá el texto antes de cambiar de destino".into(),
        );
    }
    let context = pending.context;
    let token = pending.token;
    if deliveries.pending.contains_key(&(context.project, token)) {
        fence.pending.as_mut().expect("pending fence").retain_after_enqueue_failure = true;
        return Err("No se pudo registrar la confirmación de input pendiente".into());
    }
    if !pending.bytes.is_empty()
        && let Err(error) = send(context, token, &pending.bytes, &pending.paste_ranges)
    {
        fence.pending.as_mut().expect("pending fence").retain_after_enqueue_failure = true;
        return Err(error);
    }
    let mut pending = fence.pending.take().expect("pending fence");
    if !pending.bytes.is_empty() {
        pending.batches.push_back(Batch {
            bytes: Some(Arc::new(std::mem::take(&mut pending.bytes))),
            since: now,
        });
    }
    if pending.waiting_ready || !pending.batches.is_empty() {
        deliveries.track_batches(
            context, token, pending.waiting_ready, pending.batches, pending.since,
        );
    }
    Ok(true)
}
impl Deliveries {
    fn track_batches(
        &mut self,
        context: Context,
        token: u64,
        waiting_ready: bool,
        batches: VecDeque<Batch>,
        now: Instant,
    ) -> bool {
        if (!waiting_ready && batches.is_empty()) || self.pending.contains_key(&(context.project, token)) {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending.insert(
            (context.project, token),
            Delivery {
                context,
                waiting_ready,
                batches,
                since: now,
            },
        );
        true
    }
    pub fn busy(&self, project: usize) -> bool {
        self.pending.keys().any(|(owner, _)| *owner == project)
            || self
                .failed
                .iter()
                .any(|failure| failure.context.project == project)
    }
    pub fn any_busy(&self) -> bool {
        !self.pending.is_empty() || !self.failed.is_empty()
    }
    pub fn any_warned(&self) -> bool {
        !self.warned.is_empty()
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn quit_ready(&self, override_generation: Option<u64>) -> bool {
        !self.any_busy() || (self.failed.is_empty() && override_generation == Some(self.generation))
    }
    pub fn failed_guard(&self) -> Option<FailedDeliveryGuard> {
        self.failed.front().map(|failure| FailedDeliveryGuard {
            id: failure.id,
            generation: self.generation,
        })
    }
    pub fn failed_text_if_guarded(&self, guard: FailedDeliveryGuard) -> Option<&str> {
        if guard.generation != self.generation {
            return None;
        }
        let failure = self
            .failed
            .front()
            .filter(|failure| failure.id == guard.id)?;
        std::str::from_utf8(failure.bytes.as_ref()?.as_slice()).ok()
    }
    pub fn discard_failed_guarded(&mut self, guard: FailedDeliveryGuard) -> bool {
        if guard.generation != self.generation
            || self
                .failed
                .front()
                .is_none_or(|failure| failure.id != guard.id)
        {
            return false;
        }
        let failure = self.failed.pop_front().expect("guard checked");
        self.generation = self.generation.wrapping_add(1);
        if !self.busy(failure.context.project) {
            self.warned.remove(&failure.context.project);
        }
        true
    }
    pub fn cancel_deferred(&mut self) {
        self.deferred.clear();
    }
    /// Last requested action wins while the same project is waiting.
    pub fn defer(&mut self, project: usize, action: DeferredAction) -> bool {
        if !self.busy(project) {
            return false;
        }
        if !self.warned.contains(&project) {
            self.deferred.insert(project, action);
        }
        true
    }
    /// A reply from a different attachment or a ready reply must never consume
    /// the next input batch.
    pub fn ack_for_context(&mut self, context: Context, token: u64, ok: bool) -> DeliveryAck {
        self.ack_matching(context, token, false, ok)
    }
    pub fn ack_ready_for_context(&mut self, context: Context, token: u64, ok: bool) -> DeliveryAck {
        self.ack_matching(context, token, true, ok)
    }
    fn ack_matching(&mut self, context: Context, token: u64, ready: bool, ok: bool) -> DeliveryAck {
        let project = context.project;
        let key = (project, token);
        let Some(delivery) = self.pending.get_mut(&key).filter(|delivery| {
            delivery.context == context && delivery.waiting_ready == ready
        }) else {
            return DeliveryAck::Unknown;
        };
        let bytes = if ready {
            delivery.waiting_ready = false;
            None
        } else {
            delivery.batches.pop_front().expect("tracked nonempty delivery").bytes
        };
        if let Some(next) = delivery.batches.front() {
            delivery.since = next.since;
        } else {
            self.pending.remove(&key);
        }
        self.generation = self.generation.wrapping_add(1);
        if !ok {
            if !ready {
                self.next_failure = self.next_failure.wrapping_add(1);
                self.failed.push_back(FailedDelivery {
                    id: self.next_failure,
                    context,
                    bytes,
                });
            }
            self.deferred.remove(&project);
            self.warned.insert(project);
        }
        if self.busy(project) {
            return if ok { DeliveryAck::Pending } else { DeliveryAck::Failed };
        }
        self.warned.remove(&project);
        let deferred = self.deferred.remove(&project);
        if ok { DeliveryAck::Finished(deferred) } else { DeliveryAck::Failed }
    }
    /// A timeout is not proof of non-delivery. Keep the gate closed and
    /// cancel deferred actions; a late acknowledgement still resolves it.
    pub fn warn_slow(&mut self, now: Instant) -> Vec<usize> {
        let overdue: HashSet<_> = self
            .pending
            .iter()
            .filter(|(_, delivery)| now.saturating_duration_since(delivery.since) >= TIMEOUT)
            .map(|((project, _), _)| *project)
            .collect();
        let mut fresh = Vec::new();
        for project in overdue {
            self.deferred.remove(&project);
            if self.warned.insert(project) {
                fresh.push(project);
            }
        }
        fresh
    }
}
impl Fence {
    pub fn begin(&mut self, context: Context, now: Instant) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.pending = Some(Pending {
            token: self.sequence,
            context,
            waiting_ready: true,
            bytes: Vec::new(),
            paste_ranges: Vec::new(),
            since: now,
            retain_after_enqueue_failure: false,
            halted_after_backend_failure: false,
            timed_out: false,
            rejected_waiting_ack: false,
            batches: VecDeque::new(),
            failed_batches: Vec::new(),
            raw_prefix: None,
        });
        self.sequence
    }
    pub fn active(&self) -> bool {
        self.pending.is_some()
    }
    pub fn context(&self) -> Option<Context> {
        self.pending.as_ref().map(|pending| pending.context)
    }
    /// Enqueue directly behind Resume on the same worker. UI replies only
    /// release recovery data; they never authorize the next submission.
    pub fn submit(
        &mut self,
        context: Context,
        bytes: &[u8],
        paste: bool,
        now: Instant,
        send: impl FnOnce(u64, Arc<Vec<u8>>, bool) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Some(pending) = self.pending.as_mut() else { return Ok(false); };
        Self::check_submission(pending, context)?;
        if !pending.bytes.is_empty() {
            return Err("La entrada inicial aún no se envió al backend".into());
        }
        if bytes.is_empty() { return Ok(true); }
        let bytes = Arc::new(bytes.to_vec());
        if let Err(error) = send(pending.token, Arc::clone(&bytes), paste) {
            pending.retain_after_enqueue_failure = true;
            return Err(error);
        }
        if !pending.awaiting_ack() { pending.since = now; }
        pending.batches.push_back(Batch {
            bytes: Some(bytes),
            since: now,
        });
        Ok(true)
    }
    /// Flush input held only because the backend had not attached yet.
    /// Failure is locally unsent and requires an explicit recovery decision.
    pub fn submit_buffered(
        &mut self,
        context: Context,
        now: Instant,
        send: impl FnOnce(u64, Arc<Vec<u8>>, Vec<(usize, usize)>) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Some(pending) = self.pending.as_mut() else { return Ok(false); };
        Self::check_submission(pending, context)?;
        if pending.bytes.is_empty() { return Ok(true); }
        let bytes = Arc::new(std::mem::take(&mut pending.bytes));
        let ranges = std::mem::take(&mut pending.paste_ranges);
        if let Err(error) = send(pending.token, Arc::clone(&bytes), ranges) {
            pending.raw_prefix = Some(bytes);
            pending.retain_after_enqueue_failure = true;
            pending.halted_after_backend_failure = true;
            return Err(error);
        }
        if !pending.awaiting_ack() { pending.since = now; }
        pending.batches.push_back(Batch {
            bytes: Some(bytes),
            since: now,
        });
        Ok(true)
    }
    fn check_submission(pending: &mut Pending, context: Context) -> Result<(), String> {
        if pending.context != context {
            pending.retain_after_enqueue_failure = true;
            return Err("Destino cambió durante la transición; entrada original conservada".into());
        }
        if pending.halted_after_backend_failure || pending.timed_out || pending.rejected_waiting_ack {
            return Err("Entrada original conservada; este evento no se envió".into());
        }
        Ok(())
    }
    /// Snapshot earlier input so it can be sent together with a pending copy
    /// and paste as a single ordered backend action.
    pub fn waiting_input(&self, context: Context) -> Option<(u64, Vec<u8>, Vec<(usize, usize)>)> {
        self.pending.as_ref().filter(|p| {
            p.context == context && p.raw_prefix.is_none()
                && !p.retain_after_enqueue_failure && !p.timed_out
                && !p.halted_after_backend_failure && !p.rejected_waiting_ack
        }).map(|p| (p.token, p.bytes.clone(), p.paste_ranges.clone()))
    }
    pub fn mark_ordered_paste(&mut self, context: Context, token: u64) -> bool {
        let Some(pending) = self.pending.as_mut().filter(|p| {
            p.context == context && p.token == token && p.raw_prefix.is_none()
                && !p.retain_after_enqueue_failure && !p.timed_out
                && !p.halted_after_backend_failure && !p.rejected_waiting_ack
        }) else { return false; };
        let now = Instant::now();
        if !pending.awaiting_ack() { pending.since = now; }
        pending.batches.push_back(Batch {
            bytes: (!pending.bytes.is_empty())
                .then(|| Arc::new(std::mem::take(&mut pending.bytes))),
            since: now,
        });
        pending.paste_ranges.clear();
        true
    }
    pub fn timed_out(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| p.timed_out)
    }
    pub fn awaiting_recovery_ack(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| {
                (p.timed_out || p.rejected_waiting_ack || p.halted_after_backend_failure)
                    && p.awaiting_ack()
            })
    }
    pub fn recovery_blocked(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            p.timed_out || p.rejected_waiting_ack || p.halted_after_backend_failure
        })
    }
    /// Preserve a rejected event, including its controls, without sending it
    /// to a different pane or retrying it after an uncertain acknowledgement.
    pub fn can_retain_rejected(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            !p.timed_out
                && !p.rejected_waiting_ack
                && !p.halted_after_backend_failure
                && p.raw_prefix.is_none()
        })
    }
    /// Keep an event that was rejected before any backend request was sent.
    /// Unlike retain_rejected, there is no acknowledgement to wait for.
    pub fn retain_before_backend(
        &mut self,
        context: Context,
        bytes: Vec<u8>,
        now: Instant,
    ) -> Result<(), Vec<u8>> {
        if bytes.is_empty() || self.pending.is_some() {
            return Err(bytes);
        }
        self.begin(context, now);
        let pending = self.pending.as_mut().expect("fence just created");
        pending.waiting_ready = false;
        let text = bytes.len() <= LIMIT
            && std::str::from_utf8(&bytes).is_ok()
            && !bytes
                .iter()
                .any(|byte| (*byte < 32 && !matches!(*byte, 8 | 9 | 10 | 13)) || *byte == 127);
        if text {
            pending.bytes = bytes;
        } else {
            pending.raw_prefix = Some(Arc::new(bytes));
        }
        pending.retain_after_enqueue_failure = true;
        pending.halted_after_backend_failure = true;
        Ok(())
    }
    pub fn retain_rejected(&mut self, bytes: Vec<u8>) -> Result<(), Vec<u8>> {
        if bytes.is_empty() || !self.can_retain_rejected() {
            return Err(bytes);
        }
        let p = self.pending.as_mut().expect("checked pending fence");
        p.raw_prefix = Some(Arc::new(bytes));
        p.retain_after_enqueue_failure = true;
        p.rejected_waiting_ack = p.awaiting_ack();
        p.halted_after_backend_failure = !p.rejected_waiting_ack;
        Ok(())
    }
    pub fn retained_guard(&self) -> Option<PendingGuard> {
        let pending = self.pending.as_ref().filter(|p| p.retain_after_enqueue_failure)?;
        Some(PendingGuard { pending: pending.clone(), uncertain: OnceLock::new() })
    }
    pub fn cancel_guarded(&mut self, guard: &PendingGuard) -> bool {
        if self.pending.as_ref() != Some(&guard.pending) {
            return false;
        }
        self.pending = None;
        true
    }
    /// Return only the unsent text from the exact state shown to the user.
    /// Never reinterpret partial UTF-8 or include an earlier uncertain write.
    pub fn text_if_guarded(&self, guard: &PendingGuard) -> Option<&str> {
        let pending = self.pending.as_ref().filter(|p| **p == guard.pending)?;
        if !pending.bytes.is_empty() {
            return std::str::from_utf8(&pending.bytes).ok();
        }
        // A locally rejected text event may exceed the handshake's small
        // queue limit. Permit explicit copying without treating controls or
        // an uncertain in-flight batch as plain unsent text.
        let raw = pending.raw_prefix.as_ref()?;
        if raw.len() > 2 * 1024 * 1024
            || raw
                .iter()
                .any(|b| (*b < 32 && !matches!(*b, 8 | 9 | 10 | 13)) || *b == 127)
        {
            return None;
        }
        std::str::from_utf8(raw).ok()
    }
    /// All submitted batches still lacking a successful acknowledgement, in
    /// backend order. Concatenation is done only for the explicit quit snapshot.
    pub fn uncertain_text_if_guarded<'a>(&self, guard: &'a PendingGuard) -> Option<&'a str> {
        self.pending.as_ref().filter(|p| **p == guard.pending)?;
        let uncertain = guard.uncertain.get_or_init(|| {
            let pending = &guard.pending;
            let mut bytes = pending.failed_batches.iter().chain(&pending.batches)
                .filter_map(|batch| batch.bytes.as_ref());
            bytes.next().map(|first| {
                let Some(second) = bytes.next() else { return Arc::clone(first); };
                let mut all = Vec::with_capacity(
                    first.len() + second.len() + bytes.clone().map(|batch| batch.len()).sum::<usize>(),
                );
                all.extend_from_slice(first);
                all.extend_from_slice(second);
                for batch in bytes { all.extend_from_slice(batch); }
                Arc::new(all)
            })
        });
        std::str::from_utf8(uncertain.as_ref()?.as_slice()).ok()
    }
    /// Control sequences are exposed as hex only, never replayed as input.
    /// Limit the clipboard representation; the original allocation is retained.
    pub fn prefix_hex_if_guarded(&self, guard: &PendingGuard) -> Option<String> {
        use std::fmt::Write;
        let pending = self.pending.as_ref().filter(|p| **p == guard.pending)?;
        let raw = pending.raw_prefix.as_ref()?;
        if raw.len() > LIMIT {
            return None;
        }
        let mut hex = String::with_capacity(raw.len() * 2);
        for byte in raw.iter() {
            write!(&mut hex, "{byte:02x}").ok()?;
        }
        Some(hex)
    }
    pub fn expects(&self, context: Context, token: u64, ready: bool) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            p.context == context
                && p.token == token
                && p.waiting_ready == ready
                && (ready || !p.batches.is_empty())
        })
    }
    pub fn cancel(&mut self) -> bool {
        self.pending.take().is_some_and(|p| p.has_input())
    }
    pub fn expired(&self, now: Instant) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            !p.retain_after_enqueue_failure && now.saturating_duration_since(p.since) >= TIMEOUT
        })
    }
    /// An elapsed deadline is not evidence that tmux did not execute input.
    /// Keep every outstanding batch and accept matching late acknowledgements,
    /// without replaying any bytes after the deadline.
    pub fn mark_expired(&mut self, now: Instant) -> ResumeTimeout {
        self.mark_expired_with_prefix(now, Vec::new)
    }
    /// Drain the router only after verifying expiry, and keep its exact bytes.
    pub fn mark_expired_with_prefix(
        &mut self,
        now: Instant,
        take_prefix: impl FnOnce() -> Vec<u8>,
    ) -> ResumeTimeout {
        if !self.expired(now) {
            return ResumeTimeout::NotExpired;
        }
        let prefix = take_prefix();
        let pending = self.pending.as_mut().expect("expired fence exists");
        if !prefix.is_empty() {
            pending.raw_prefix = Some(Arc::new(prefix));
        }
        if !pending.has_input() {
            self.pending = None;
            return ResumeTimeout::NoInput;
        }
        pending.timed_out = true;
        pending.retain_after_enqueue_failure = true;
        ResumeTimeout::Retained
    }
    /// Retain input only while its backend has not attached yet.
    pub fn push(&mut self, context: Context, bytes: &[u8]) -> Result<bool, String> {
        let Some(p) = self.pending.as_mut() else {
            return Ok(false);
        };
        if p.context != context {
            p.retain_after_enqueue_failure = true;
            return Err("Destino cambió durante la transición; entrada original conservada".into());
        }
        if p.halted_after_backend_failure || p.timed_out || p.rejected_waiting_ack {
            return Err("Entrada original conservada; este evento no se envió".into());
        }
        p.bytes.extend_from_slice(bytes);
        Ok(true)
    }
    /// Record paste boundaries separately from their bytes so the backend can
    /// use the destination application's current bracketed-paste mode.
    pub fn push_paste(&mut self, context: Context, bytes: &[u8]) -> Result<bool, String> {
        let start = self.pending.as_ref().map(|pending| pending.bytes.len()).unwrap_or(0);
        if !self.push(context, bytes)? { return Ok(false); }
        if !bytes.is_empty() {
            self.pending.as_mut().expect("paste accepted").paste_ranges.push((start, start + bytes.len()));
        }
        Ok(true)
    }
    /// On an error the caller changes focus. Capture any partially routed
    /// terminal key before changing that focus and before releasing the fence.
    /// Stale acknowledgements must not change focus or consume the prefix.
    pub fn ack_with_failure_prefix(
        &mut self,
        context: Context,
        token: u64,
        ready: bool,
        ok: bool,
        take_prefix: impl FnOnce() -> Vec<u8>,
    ) -> bool {
        if !self.expects(context, token, ready) {
            return false;
        }
        if !ok && !self.recovery_blocked() {
            let prefix = take_prefix();
            if !prefix.is_empty() {
                let pending = self.pending.as_mut().expect("matching fence exists");
                debug_assert!(pending.raw_prefix.is_none());
                pending.raw_prefix = Some(Arc::new(prefix));
            }
        }
        let p = self.pending.as_mut().expect("matching fence exists");
        let was_blocked = p.timed_out || p.rejected_waiting_ack || p.halted_after_backend_failure;
        if ready {
            p.waiting_ready = false;
        } else {
            let batch = p.batches.pop_front().expect("expected FIFO acknowledgement");
            if !ok { p.failed_batches.push(batch); }
        }
        if let Some(next) = p.batches.front() { p.since = next.since; }
        if !ok || was_blocked {
            if !p.has_input() && !p.awaiting_ack() {
                self.pending = None;
            } else {
                p.retain_after_enqueue_failure = true;
                p.halted_after_backend_failure = true;
                if !p.awaiting_ack() {
                    p.timed_out = false;
                    p.rejected_waiting_ack = false;
                }
            }
            return true;
        }
        // A ready reply or an earlier batch must not drain data that is
        // already queued on the worker, nor release direct PTY writes early.
        if !p.batches.is_empty() { return true; }
        if p.bytes.is_empty() {
            self.pending = None;
            return true;
        }
        true
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> Context {
        Context { project: 1, attachment: 5 }
    }

    #[test]
    fn unsent_input_during_reattach_is_retained_without_waiting_for_an_ack() {
        let context = Context { project: 2, attachment: 7 };
        let mut fence = Fence::default();
        fence.retain_before_backend(context, b"synthetic input".to_vec(), Instant::now()).unwrap();
        assert!(fence.active());
        assert!(fence.recovery_blocked());
        assert!(!fence.awaiting_recovery_ack());
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("synthetic input"));
        assert!(fence.retain_before_backend(context, b"new".to_vec(), Instant::now()).is_err());
        assert!(fence.cancel_guarded(&guard));
        assert!(!fence.active());
        fence.retain_before_backend(context, b"\x1b[A".to_vec(), Instant::now()).unwrap();
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), None);
        assert_eq!(fence.prefix_hex_if_guarded(&guard).as_deref(), Some("1b5b41"));
        assert!(fence.cancel_guarded(&guard));
    }

    #[test]
    fn detached_ack_requires_the_original_attachment() {
        let now = Instant::now();
        let original = Context { project: 4, attachment: 77 };
        let other = Context { project: 4, attachment: 78 };
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(original, now);
        fence.submit(original, b"preserved", false, now, |_, _, _| Ok(())).unwrap();
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            panic!("already submitted")
        }).unwrap());
        assert_eq!(deliveries.ack_ready_for_context(other, token, true), DeliveryAck::Unknown);
        assert_eq!(deliveries.ack_for_context(original, token, true), DeliveryAck::Unknown);
        assert_eq!(deliveries.ack_ready_for_context(original, token, true), DeliveryAck::Pending);
        assert_eq!(deliveries.ack_for_context(other, token, true), DeliveryAck::Unknown);
        assert!(deliveries.busy(original.project));
        assert_eq!(deliveries.ack_for_context(original, token, true), DeliveryAck::Finished(None));
        assert!(!deliveries.busy(original.project));
    }

    #[test]
    fn quit_override_cannot_discard_input_that_changed_after_prompt() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        assert!(fence.retained_guard().is_none());
        fence.push(context(), b"first").unwrap();
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            Err("full".into())
        }).is_err());
        let stale = fence.retained_guard().unwrap();
        fence.push(context(), b"-later").unwrap();
        assert!(!fence.cancel_guarded(&stale));
        let current = fence.retained_guard().unwrap();
        assert!(fence.cancel_guarded(&current));
        assert!(!fence.active());
        assert!(!fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
    }

    #[test]
    fn quit_override_expires_when_pending_input_is_accepted() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"first").unwrap();
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            Err("full".into())
        }).is_err());
        let stale = fence.retained_guard().unwrap();
        assert!(fence.submit_buffered(context(), now, |id, bytes, ranges| {
            assert_eq!(id, token);
            assert_eq!(bytes.as_slice(), b"first");
            assert!(ranges.is_empty());
            Ok(())
        }).unwrap());
        assert!(!fence.cancel_guarded(&stale));
        assert!(fence.active());
    }

    #[test]
    fn failed_detach_enqueue_preserves_buffer_and_ready_ack_path() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"kept").unwrap();
        let failure = detach_with_delivery_typed(
            &mut fence, &mut deliveries, now, |owner, id, bytes, ranges| {
                assert_eq!((owner, id, bytes), (context(), token, b"kept".as_slice()));
                assert!(ranges.is_empty());
                Err("queue full".into())
            },
        );
        assert_eq!(failure, Err("queue full".into()));
        assert!(fence.active());
        assert!(!deliveries.busy(1));
        assert!(!fence.expired(now + TIMEOUT + Duration::from_secs(1)));
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("kept"));
        assert_eq!(fence.uncertain_text_if_guarded(&guard), None);
    }

    #[test]
    fn failed_detach_enqueue_preserves_inflight_ack_and_later_bytes() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.submit(context(), b"first", false, now, |_, _, _| Ok(())).unwrap();
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        fence.push(context(), b"second").unwrap();
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            Err("queue full".into())
        }).is_err());
        assert!(!deliveries.busy(1));
        assert!(fence.expects(context(), token, false));
        assert!(fence.ack_with_failure_prefix(context(), token, false, true, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("second"));
        assert_eq!(fence.uncertain_text_if_guarded(&guard), None);
    }

    #[test]
    fn accepted_detach_tracks_once_and_releases_fence() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.push_paste(context(), b"kept").unwrap();
        let mut sent = Vec::new();
        assert_eq!(detach_with_delivery_typed(
            &mut fence, &mut deliveries, now, |owner, id, bytes, ranges| {
                assert_eq!((owner, id), (context(), token));
                assert_eq!(ranges, &[(0, 4)]);
                sent.extend_from_slice(bytes);
                Ok(())
            },
        ), Ok(true));
        assert_eq!(sent, b"kept");
        assert!(!fence.active());
        assert!(deliveries.busy(1));
        assert_eq!(deliveries.ack_ready_for_context(context(), token, true), DeliveryAck::Pending);
        assert_eq!(deliveries.ack_for_context(context(), token, true), DeliveryAck::Finished(None));
    }

    #[test]
    fn detached_failed_batches_remain_separate_and_are_never_automatically_replayed() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.submit(context(), b"first possibly sent", false, now, |_, _, _| Ok(())).unwrap();
        fence.submit(context(), b"second possibly sent", true, now, |_, _, _| Ok(())).unwrap();
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            panic!("already submitted")
        }).unwrap());
        assert_eq!(deliveries.ack_ready_for_context(context(), token, true), DeliveryAck::Pending);
        assert_eq!(deliveries.ack_for_context(context(), token, false), DeliveryAck::Failed);
        let stale = deliveries.failed_guard().unwrap();
        assert_eq!(deliveries.failed_text_if_guarded(stale), Some("first possibly sent"));
        assert!(!deliveries.quit_ready(Some(deliveries.generation())));
        assert_eq!(deliveries.ack_for_context(context(), token, false), DeliveryAck::Failed);
        assert_eq!(deliveries.failed_text_if_guarded(stale), None);
        assert!(!deliveries.discard_failed_guarded(stale));
        let first = deliveries.failed_guard().unwrap();
        assert_eq!(deliveries.failed_text_if_guarded(first), Some("first possibly sent"));
        assert!(deliveries.discard_failed_guarded(first));
        let second = deliveries.failed_guard().unwrap();
        assert_eq!(deliveries.failed_text_if_guarded(second), Some("second possibly sent"));
        assert!(deliveries.discard_failed_guarded(second));
        assert!(!deliveries.any_busy());
        assert_eq!(deliveries.ack_for_context(context(), token, false), DeliveryAck::Unknown);
    }

    #[test]
    fn terminal_delete_waits_for_resume_and_its_project_deliveries() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.submit(context(), b"queued", false, now, |_, _, _| Ok(())).unwrap();
        assert!(terminal_delete_busy(&fence, &deliveries, 1, false, false, false));
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            panic!("already submitted")
        }).unwrap());
        assert_eq!(deliveries.ack_ready_for_context(context(), token, true), DeliveryAck::Pending);
        assert!(terminal_delete_busy(&fence, &deliveries, 1, false, false, false));
        assert!(!terminal_delete_busy(&fence, &deliveries, 2, false, false, false));
        let other = Context { project: 2, ..context() };
        assert_eq!(deliveries.ack_for_context(other, token, true), DeliveryAck::Unknown);
        assert!(terminal_delete_busy(&fence, &deliveries, 1, false, false, false));
        assert_eq!(deliveries.ack_for_context(context(), token, true), DeliveryAck::Finished(None));
        assert!(!terminal_delete_busy(&fence, &deliveries, 1, false, false, false));
        for (search, rebind, open) in [(true, false, false), (false, true, false), (false, false, true)] {
            assert!(terminal_delete_busy(&fence, &deliveries, 1, search, rebind, open));
        }
    }

    #[test]
    fn deferred_session_actions_wait_for_every_delivery_and_are_project_scoped() {
        let now = Instant::now();
        let mut deliveries = Deliveries::default();
        let other = Context { project: 2, ..context() };
        assert!(!deliveries.track_batches(context(), 10, false, VecDeque::new(), now));
        assert!(deliveries.track_batches(context(), 10, false, vec![
            Batch { bytes: None, since: now }, Batch { bytes: None, since: now },
        ].into(), now));
        assert!(!deliveries.track_batches(context(), 10, false,
            vec![Batch { bytes: None, since: now }].into(), now));
        assert!(deliveries.track_batches(context(), 11, false,
            vec![Batch { bytes: None, since: now }].into(), now));
        assert!(deliveries.track_batches(other, 12, false,
            vec![Batch { bytes: None, since: now }].into(), now));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert!(deliveries.defer(1, DeferredAction::CloseWorkspace));
        assert!(!deliveries.defer(3, DeferredAction::Reconnect));
        assert_eq!(deliveries.ack_for_context(other, 10, true), DeliveryAck::Unknown);
        assert_eq!(deliveries.ack_for_context(context(), 10, true), DeliveryAck::Pending);
        assert_eq!(deliveries.ack_for_context(context(), 11, true), DeliveryAck::Pending);
        assert_eq!(deliveries.ack_for_context(context(), 10, true),
            DeliveryAck::Finished(Some(DeferredAction::CloseWorkspace)));
        assert!(!deliveries.busy(1));
        assert!(deliveries.busy(2));
        assert_eq!(deliveries.ack_for_context(other, 12, true), DeliveryAck::Finished(None));
    }

    #[test]
    fn failure_and_timeout_cancel_deferred_actions_without_assuming_non_delivery() {
        let now = Instant::now();
        let mut deliveries = Deliveries::default();
        assert!(deliveries.track_batches(context(), 20, false, vec![
            Batch { bytes: None, since: now }, Batch { bytes: None, since: now },
        ].into(), now));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert_eq!(deliveries.ack_for_context(context(), 20, false), DeliveryAck::Failed);
        assert!(deliveries.busy(1));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert_eq!(deliveries.ack_for_context(context(), 20, true), DeliveryAck::Pending);
        let failed = deliveries.failed_guard().unwrap();
        assert_eq!(deliveries.failed_text_if_guarded(failed), None);
        assert!(deliveries.discard_failed_guarded(failed));
        assert!(!deliveries.busy(1));
        assert!(deliveries.track_batches(context(), 21, false,
            vec![Batch { bytes: None, since: now }].into(), now));
        assert!(deliveries.defer(1, DeferredAction::CloseWorkspace));
        assert!(deliveries.warn_slow(now + TIMEOUT - Duration::from_nanos(1)).is_empty());
        assert_eq!(deliveries.warn_slow(now + TIMEOUT), vec![1]);
        assert!(deliveries.warn_slow(now + TIMEOUT).is_empty());
        assert!(deliveries.busy(1));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert_eq!(deliveries.ack_for_context(context(), 21, true), DeliveryAck::Finished(None));
        assert!(!deliveries.busy(1));
    }

    #[test]
    fn quit_override_covers_only_the_deliveries_the_user_was_warned_about() {
        let now = Instant::now();
        let mut deliveries = Deliveries::default();
        let other = Context { project: 2, ..context() };
        assert!(deliveries.quit_ready(None));
        assert!(deliveries.track_batches(context(), 10, false,
            vec![Batch { bytes: None, since: now }].into(), now));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert!(!deliveries.quit_ready(None));
        let approved = deliveries.generation();
        assert!(deliveries.quit_ready(Some(approved)));
        deliveries.cancel_deferred();
        assert!(deliveries.track_batches(other, 20, false,
            vec![Batch { bytes: None, since: now }].into(), now));
        assert!(!deliveries.quit_ready(Some(approved)));
        assert_eq!(deliveries.ack_for_context(context(), 10, true), DeliveryAck::Finished(None));
        assert!(!deliveries.quit_ready(Some(approved)));
        assert_eq!(deliveries.ack_for_context(other, 20, true), DeliveryAck::Finished(None));
        assert!(deliveries.quit_ready(None));
    }

    #[test]
    fn quit_intent_waits_and_user_activity_invalidates_a_scheduled_retry() {
        let mut intent = QuitIntent::default();
        intent.request();
        assert_eq!(intent.take_ready(true), None);
        let retry = intent.take_ready(false).unwrap();
        assert!(intent.is_current(retry));
        intent.activity();
        assert!(!intent.is_current(retry));
        intent.request();
        intent.activity();
        assert_eq!(intent.take_ready(false), None);
        intent.request();
        intent.cancel();
        assert_eq!(intent.take_ready(false), None);
        intent.request();
        let canceled = intent.take_ready(false).unwrap();
        intent.cancel();
        assert!(!intent.is_current(canceled));
    }

    #[test]
    fn rejected_control_waits_for_ack_without_sending_any_later_batch() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"before").unwrap();
        fence.retain_rejected(b"\x1bx".to_vec()).unwrap();
        assert!(fence.recovery_blocked());
        assert!(fence.awaiting_recovery_ack());
        assert!(fence.push(context(), b"after").is_err());
        assert!(fence.retain_rejected(b"second".to_vec()).is_err());
        assert!(!fence.ack_with_failure_prefix(context(), token + 1, true, true, Vec::new));
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("before"));
        assert_eq!(fence.prefix_hex_if_guarded(&guard).as_deref(), Some("1b78"));
        assert!(fence.recovery_blocked());
        let mut deliveries = Deliveries::default();
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            panic!("must not send")
        }).is_err());
        assert!(fence.cancel_guarded(&guard));
    }

    #[test]
    fn rejected_control_keeps_unsent_after_inflight_success_or_failure() {
        let now = Instant::now();
        for ok in [true, false] {
            let mut fence = Fence::default();
            let token = fence.begin(context(), now);
            fence.submit(context(), b"first", false, now, |_, _, _| Ok(())).unwrap();
            assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
            fence.push(context(), b"later").unwrap();
            fence.retain_rejected(b"\x1c".to_vec()).unwrap();
            assert!(fence.ack_with_failure_prefix(context(), token, false, ok, Vec::new));
            let guard = fence.retained_guard().unwrap();
            assert_eq!(fence.text_if_guarded(&guard), Some("later"));
            assert_eq!(fence.prefix_hex_if_guarded(&guard).as_deref(), Some("1c"));
            assert_eq!(fence.uncertain_text_if_guarded(&guard), (!ok).then_some("first"));
        }
    }

    #[test]
    fn immediate_text_submits_before_ready_and_preserves_fifo_ack_order() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        let mut submitted = Vec::new();
        fence.submit(context(), b"first\r", false, now, |id, bytes, paste| {
            assert_eq!((id, paste), (token, false));
            submitted.push(bytes);
            Ok(())
        }).unwrap();
        assert!(fence.mark_ordered_paste(context(), token));
        fence.submit(context(), "ñ second".as_bytes(), true, now, |id, bytes, paste| {
            assert_eq!((id, paste), (token, true));
            submitted.push(bytes);
            Ok(())
        }).unwrap();
        assert_eq!(submitted.len(), 2);
        assert_eq!(submitted[0].as_slice(), b"first\r");
        assert_eq!(submitted[1].as_slice(), "ñ second".as_bytes());
        assert!(!fence.ack_with_failure_prefix(context(), token, false, true, Vec::new));
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        for _ in 0..2 {
            assert!(fence.ack_with_failure_prefix(context(), token, false, true, Vec::new));
            assert!(fence.active());
        }
        assert!(fence.ack_with_failure_prefix(context(), token, false, true, Vec::new));
        assert!(!fence.active());
        assert_eq!(fence.submit(context(), b"direct", false, now, |_, _, _| {
            panic!("no active fence")
        }), Ok(false));
    }

    #[test]
    fn timeout_before_ready_keeps_unsent_input_and_late_ready_does_not_send_it() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"queued").unwrap();
        assert_eq!(fence.mark_expired(now + TIMEOUT - Duration::from_nanos(1)), ResumeTimeout::NotExpired);
        assert_eq!(fence.mark_expired(now + TIMEOUT), ResumeTimeout::Retained);
        let stale = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&stale), Some("queued"));
        assert!(fence.push(context(), b"new").is_err());
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            panic!("must not enqueue after timeout")
        }).is_err());
        assert!(!fence.ack_with_failure_prefix(context(), token + 1, true, true, Vec::new));
        assert!(fence.timed_out());
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        assert!(!fence.cancel_guarded(&stale));
        let current = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&current), Some("queued"));
        assert!(!fence.timed_out());
        assert!(fence.cancel_guarded(&current));
    }

    #[test]
    fn late_write_ack_resolves_only_the_earlier_batch() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        fence.submit(context(), b"sent earlier", false, now, |_, _, _| Ok(())).unwrap();
        fence.submit(context(), b"sent later", false, now, |_, _, _| Ok(())).unwrap();
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        assert_eq!(fence.mark_expired(now + TIMEOUT), ResumeTimeout::Retained);
        let old = fence.retained_guard().unwrap();
        assert_eq!(fence.uncertain_text_if_guarded(&old), Some("sent earliersent later"));
        assert!(fence.ack_with_failure_prefix(context(), token, false, true, Vec::new));
        assert!(!fence.cancel_guarded(&old));
        let new = fence.retained_guard().unwrap();
        assert_eq!(fence.uncertain_text_if_guarded(&new), Some("sent later"));
        assert_eq!(fence.text_if_guarded(&new), None);
        assert!(fence.cancel_guarded(&new));
    }

    #[test]
    fn timeout_with_no_input_clears_and_late_error_retains_uncertain_input() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let old = fence.begin(context(), now);
        assert_eq!(fence.mark_expired(now + TIMEOUT), ResumeTimeout::NoInput);
        assert!(!fence.active());
        assert!(!fence.ack_with_failure_prefix(context(), old, true, true, Vec::new));
        let token = fence.begin(context(), now);
        fence.submit(context(), b"possibly sent", false, now, |_, _, _| Ok(())).unwrap();
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        assert_eq!(fence.mark_expired(now + TIMEOUT), ResumeTimeout::Retained);
        assert!(fence.ack_with_failure_prefix(context(), token, false, false, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.uncertain_text_if_guarded(&guard), Some("possibly sent"));
        assert!(fence.cancel_guarded(&guard));
    }

    #[test]
    fn cancel_error_timeout_and_other_project_or_attachment_never_drain_bytes() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let old = fence.begin(context(), now);
        fence.submit(context(), b"old", false, now, |_, _, _| Ok(())).unwrap();
        assert!(fence.cancel());
        let token = fence.begin(context(), now);
        fence.submit(context(), b"new", false, now, |_, _, _| Ok(())).unwrap();
        for (owner, id) in [
            (context(), old),
            (Context { project: 2, ..context() }, token),
            (Context { attachment: 6, ..context() }, token),
        ] {
            assert!(!fence.ack_with_failure_prefix(owner, id, true, true, Vec::new));
        }
        assert!(!fence.expired(now));
        assert!(fence.expired(now + TIMEOUT));
        assert!(fence.ack_with_failure_prefix(context(), token, true, false, Vec::new));
        let guard = fence.retained_guard().expect("submitted input is retained");
        assert_eq!(fence.uncertain_text_if_guarded(&guard), Some("new"));
        assert!(!fence.expects(context(), token, true));
        assert!(!fence.expired(now + TIMEOUT + TIMEOUT));
        assert!(fence.cancel_guarded(&guard));
        assert!(!fence.active());
    }

    #[test]
    fn wrong_context_keeps_queued_bytes_for_the_original_terminal() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let original = context();
        let token = fence.begin(original, now);
        fence.push(original, b"pending").unwrap();
        let other = Context { project: original.project + 1, ..original };
        assert!(fence.submit(other, b"must-not-be-sent", false, now, |_, _, _| {
            panic!("wrong destination")
        }).is_err());
        assert!(fence.retained_guard().is_some());
        assert!(!fence.expired(now + TIMEOUT));
        assert!(fence.ack_with_failure_prefix(original, token, true, true, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("pending"));
    }

    #[test]
    fn copying_requires_current_snapshot_and_valid_utf8() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        fence.begin(context(), now);
        fence.push(context(), b"before").unwrap();
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            Err("full".into())
        }).is_err());
        let old = fence.retained_guard().unwrap();
        fence.push(context(), b"-after").unwrap();
        assert_eq!(fence.text_if_guarded(&old), None);
        let current = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&current), Some("before-after"));
        fence.push(context(), &[0xf0]).unwrap();
        let partial = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&partial), None);
        assert!(fence.cancel_guarded(&partial));
    }

    #[test]
    fn failed_ready_ack_retains_unsent_input_without_retry_or_retarget() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"not sent").unwrap();
        assert!(fence.ack_with_failure_prefix(context(), token, true, false, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("not sent"));
        assert!(fence.push(context(), b"new event").is_err());
        assert!(detach_with_delivery_typed(&mut fence, &mut deliveries, now, |_, _, _, _| {
            panic!("failed transition must not send or retarget bytes")
        }).is_err());
        assert!(!fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        assert_eq!(fence.text_if_guarded(&guard), Some("not sent"));
        assert!(fence.cancel_guarded(&guard));
    }

    #[test]
    fn failed_write_retains_every_submitted_batch_without_replaying_any_of_them() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        fence.submit(context(), b"possibly sent", false, now, |_, _, _| Ok(())).unwrap();
        fence.submit(context(), b"dependent later text", false, now, |_, _, _| Ok(())).unwrap();
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        assert!(fence.ack_with_failure_prefix(context(), token, false, false, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), None);
        assert_eq!(fence.uncertain_text_if_guarded(&guard), Some("possibly sentdependent later text"));
        assert!(fence.submit(context(), b"new event", false, now, |_, _, _| {
            panic!("failed chain cannot be retried")
        }).is_err());
        assert!(fence.ack_with_failure_prefix(context(), token, false, false, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.uncertain_text_if_guarded(&guard), Some("possibly sentdependent later text"));
        assert!(fence.cancel_guarded(&guard));
    }

    #[test]
    fn failed_write_without_later_input_retains_the_uncertain_batch() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        fence.submit(context(), b"possibly executed", false, now, |_, _, _| Ok(())).unwrap();
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        assert!(fence.ack_with_failure_prefix(context(), token, false, false, Vec::new));
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), None);
        assert_eq!(fence.uncertain_text_if_guarded(&guard), Some("possibly executed"));
        assert!(fence.cancel_guarded(&guard));
    }

    #[test]
    fn startup_buffer_keeps_controls_and_large_events_for_the_original_target() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        let mut text = vec![b'x'; LIMIT];
        text.extend_from_slice(b"extra\x01\x1b[200~paste\x1b[201~");
        fence.push(context(), &text).unwrap();
        fence.submit_buffered(context(), now, |id, bytes, ranges| {
            assert_eq!(id, token);
            assert_eq!(bytes.as_slice(), text.as_slice());
            assert!(ranges.is_empty());
            Ok(())
        }).unwrap();
        assert!(fence.submit(Context { project: 2, ..context() }, b"wrong", false, now, |_, _, _| {
            panic!("wrong target")
        }).is_err());
        assert!(fence.ack_with_failure_prefix(context(), token, true, true, Vec::new));
        assert!(fence.active());
        assert!(fence.ack_with_failure_prefix(context(), token, false, true, Vec::new));
        assert!(!fence.active());
    }
}
