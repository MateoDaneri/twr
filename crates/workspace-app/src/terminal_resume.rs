//! Bounded resume handshake. No bytes leave this gate before backend acknowledgement.
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
const LIMIT: usize = 4096;
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DELIVERY_BATCHES: usize = 32;
const MAX_DELIVERY_BYTES: usize = MAX_DELIVERY_BATCHES * LIMIT * 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Context {
    pub project: usize,
    pub attachment: u64,
}
#[derive(Clone, PartialEq, Eq)]
struct Pending {
    token: u64,
    context: Context,
    waiting_ready: bool,
    retrying_enqueue: bool,
    bytes: Vec<u8>,
    since: Instant,
    retain_after_enqueue_failure: bool,
    halted_after_backend_failure: bool,
    timed_out: bool,
    rejected_waiting_ack: bool,
    in_flight: Option<Vec<u8>>,
    raw_prefix: Option<Arc<Vec<u8>>>,
}
/// Opaque snapshot used to avoid discarding newer input after a quit prompt.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingGuard(Pending);
#[derive(Default)]
pub struct Fence {
    sequence: u64,
    pending: Option<Pending>,
}
/// A focus change can hand the queued bytes to the same backend, while keeping
/// the original token. This does not authorize delivery: the backend must still
/// verify the tmux window and pane associated with that token.
#[derive(Debug, PartialEq, Eq)]
pub struct Detached {
    pub context: Context,
    pub token: u64,
    pub bytes: Vec<u8>,
    pub in_flight: bool,
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
    batches: VecDeque<Option<Vec<u8>>>,
    since: Instant,
}

struct FailedDelivery {
    id: u64,
    context: Context,
    bytes: Option<Vec<u8>>,
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

/// A terminal router prefix still belongs ahead of any focus change. Do not
/// detach its earlier queued input until the resume has completed and the
/// router can deliver that prefix through the original terminal client.
pub fn router_prefix_blocks_detach(fence: &Fence, terminal: bool, pending: bool) -> bool {
    fence.active() && terminal && pending
}

/// Transfer queued input only after it has been accepted by the backend. A
/// rejected enqueue leaves the fence and its original destination untouched.
pub fn detach_with_delivery(
    fence: &mut Fence,
    deliveries: &mut Deliveries,
    now: Instant,
    send: impl FnOnce(Context, u64, &[u8]) -> Result<(), String>,
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
    let mut batches = VecDeque::new();
    if !pending.waiting_ready && !pending.retrying_enqueue {
        batches.push_back(pending.in_flight.clone());
    }
    if !pending.bytes.is_empty() {
        batches.push_back(Some(pending.bytes.clone()));
    }
    if !batches.is_empty() && !deliveries.track_batches(context, token, batches, now) {
        fence
            .pending
            .as_mut()
            .expect("pending fence")
            .retain_after_enqueue_failure = true;
        return Err("No se pudo registrar la confirmación de input pendiente".into());
    }
    if !pending.bytes.is_empty()
        && let Err(error) = send(context, token, &pending.bytes)
    {
        deliveries.pending.remove(&(context.project, token));
        fence
            .pending
            .as_mut()
            .expect("pending fence")
            .retain_after_enqueue_failure = true;
        return Err(error);
    }
    fence.detach();
    Ok(true)
}
impl Deliveries {
    #[cfg(test)]
    pub fn track(&mut self, project: usize, token: u64, count: usize, now: Instant) -> bool {
        self.track_batches(
            Context {
                project,
                attachment: 0,
            },
            token,
            std::iter::repeat_n(None, count).collect(),
            now,
        )
    }
    fn track_batches(
        &mut self,
        context: Context,
        token: u64,
        batches: VecDeque<Option<Vec<u8>>>,
        now: Instant,
    ) -> bool {
        let current_count: usize = self
            .pending
            .values()
            .map(|d| d.batches.len())
            .sum::<usize>()
            + self.failed.len();
        let current_bytes: usize = self
            .pending
            .values()
            .flat_map(|d| &d.batches)
            .chain(self.failed.iter().map(|f| &f.bytes))
            .filter_map(Option::as_ref)
            .map(Vec::len)
            .sum();
        let new_bytes: usize = batches
            .iter()
            .filter_map(Option::as_ref)
            .map(Vec::len)
            .sum();
        if batches.is_empty()
            || self.pending.contains_key(&(context.project, token))
            || current_count.saturating_add(batches.len()) > MAX_DELIVERY_BATCHES
            || current_bytes.saturating_add(new_bytes) > MAX_DELIVERY_BYTES
        {
            return false;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending.insert(
            (context.project, token),
            Delivery {
                context,
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
        std::str::from_utf8(failure.bytes.as_ref()?).ok()
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
    /// A reply from a different attachment must not consume this delivery,
    /// even if its project and token happen to match.
    pub fn ack_for_context(&mut self, context: Context, token: u64, ok: bool) -> DeliveryAck {
        if self
            .pending
            .get(&(context.project, token))
            .is_none_or(|delivery| delivery.context != context)
        {
            return DeliveryAck::Unknown;
        }
        self.ack(context.project, token, ok)
    }

    pub fn ack(&mut self, project: usize, token: u64, ok: bool) -> DeliveryAck {
        let key = (project, token);
        let Some(delivery) = self.pending.get_mut(&key) else {
            return DeliveryAck::Unknown;
        };
        let context = delivery.context;
        let bytes = delivery
            .batches
            .pop_front()
            .expect("tracked nonempty delivery");
        if delivery.batches.is_empty() {
            self.pending.remove(&key);
        }
        self.generation = self.generation.wrapping_add(1);
        if !ok {
            self.next_failure = self.next_failure.wrapping_add(1);
            self.failed.push_back(FailedDelivery {
                id: self.next_failure,
                context,
                bytes,
            });
            self.deferred.remove(&project);
            self.warned.insert(project);
        }
        if self.busy(project) {
            return if ok {
                DeliveryAck::Pending
            } else {
                DeliveryAck::Failed
            };
        }
        self.warned.remove(&project);
        let deferred = self.deferred.remove(&project);
        if ok {
            DeliveryAck::Finished(deferred)
        } else {
            DeliveryAck::Failed
        }
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
            retrying_enqueue: false,
            bytes: Vec::new(),
            since: now,
            retain_after_enqueue_failure: false,
            halted_after_backend_failure: false,
            timed_out: false,
            rejected_waiting_ack: false,
            in_flight: None,
            raw_prefix: None,
        });
        self.sequence
    }
    pub fn active(&self) -> bool {
        self.pending.is_some()
    }
    pub fn timed_out(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| p.timed_out)
    }
    pub fn awaiting_recovery_ack(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.timed_out || p.rejected_waiting_ack)
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
        if p.retrying_enqueue {
            // This batch was rejected locally and has no outstanding ack.
            p.halted_after_backend_failure = true;
        } else {
            p.rejected_waiting_ack = true;
        }
        Ok(())
    }
    pub fn retained_guard(&self) -> Option<PendingGuard> {
        self.pending
            .as_ref()
            .filter(|p| p.retain_after_enqueue_failure)
            .cloned()
            .map(PendingGuard)
    }
    pub fn cancel_guarded(&mut self, guard: &PendingGuard) -> bool {
        if self.pending.as_ref() != Some(&guard.0) {
            return false;
        }
        self.pending = None;
        true
    }
    /// Return only the unsent text from the exact state shown to the user.
    /// Never reinterpret partial UTF-8 or include an earlier uncertain write.
    pub fn text_if_guarded(&self, guard: &PendingGuard) -> Option<&str> {
        let pending = self.pending.as_ref().filter(|p| **p == guard.0)?;
        if !pending.bytes.is_empty() {
            return std::str::from_utf8(&pending.bytes).ok();
        }
        // A locally rejected text event may exceed the handshake's small
        // queue limit. Permit explicit copying without treating controls or
        // an uncertain in-flight batch as plain unsent text.
        let raw = pending.raw_prefix.as_ref()?;
        if pending.in_flight.is_some()
            || raw.len() > 2 * 1024 * 1024
            || raw
                .iter()
                .any(|b| (*b < 32 && !matches!(*b, 8 | 9 | 10 | 13)) || *b == 127)
        {
            return None;
        }
        std::str::from_utf8(raw).ok()
    }
    /// This earlier batch reached the backend, but may or may not have run.
    pub fn uncertain_text_if_guarded(&self, guard: &PendingGuard) -> Option<&str> {
        let pending = self.pending.as_ref().filter(|p| **p == guard.0)?;
        std::str::from_utf8(pending.in_flight.as_ref()?).ok()
    }
    /// Control sequences are exposed as hex only, never replayed as input.
    /// Limit the clipboard representation; the original allocation is retained.
    pub fn prefix_hex_if_guarded(&self, guard: &PendingGuard) -> Option<String> {
        use std::fmt::Write;
        let pending = self.pending.as_ref().filter(|p| **p == guard.0)?;
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
                && !p.halted_after_backend_failure
                && (ready || !p.retrying_enqueue)
        })
    }
    pub fn cancel(&mut self) -> bool {
        self.pending.take().is_some_and(|p| !p.bytes.is_empty())
    }
    pub fn detach(&mut self) -> Option<Detached> {
        let pending = self.pending.take()?;
        Some(Detached {
            context: pending.context,
            token: pending.token,
            bytes: pending.bytes,
            in_flight: !pending.waiting_ready && !pending.retrying_enqueue,
        })
    }
    pub fn expired(&self, now: Instant) -> bool {
        self.pending.as_ref().is_some_and(|p| {
            !p.retain_after_enqueue_failure && now.saturating_duration_since(p.since) >= TIMEOUT
        })
    }
    /// An elapsed deadline is not evidence that tmux did not execute input.
    /// Keep both batches and accept a matching late acknowledgement, but do
    /// not enqueue later text automatically after the deadline.
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
        if pending.bytes.is_empty() && pending.in_flight.is_none() && pending.raw_prefix.is_none() {
            self.pending = None;
            return ResumeTimeout::NoInput;
        }
        pending.timed_out = true;
        pending.retain_after_enqueue_failure = true;
        ResumeTimeout::Retained
    }
    /// A rejected local enqueue cannot have delivered these bytes. Keep them
    /// ahead of later keystrokes and retry only against the same attachment.
    pub fn retain_unsent(&mut self, context: Context, token: u64, bytes: Vec<u8>) -> bool {
        let Some(pending) = self.pending.as_mut().filter(|p| {
            p.context == context
                && p.token == token
                && !p.waiting_ready
                && !p.halted_after_backend_failure
                && !p.timed_out
                && !p.rejected_waiting_ack
        }) else {
            return false;
        };
        let mut combined = bytes;
        pending.in_flight = None;
        combined.append(&mut pending.bytes);
        pending.bytes = combined;
        pending.retrying_enqueue = true;
        pending.retain_after_enqueue_failure = true;
        true
    }
    pub fn take_retry(&mut self) -> Option<(Context, u64, Vec<u8>)> {
        let pending = self.pending.as_mut().filter(|p| {
            p.retrying_enqueue
                && !p.halted_after_backend_failure
                && !p.timed_out
                && !p.rejected_waiting_ack
        })?;
        pending.retrying_enqueue = false;
        let bytes = std::mem::take(&mut pending.bytes);
        pending.in_flight = Some(bytes.clone());
        Some((pending.context, pending.token, bytes))
    }
    /// During the short handshake, accept ordinary text and line editing only.
    /// Prefix/escape/mouse sequences must not be reinterpreted as direct pane input.
    /// Reject the whole event on overflow or unsupported control bytes.
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
        if bytes
            .iter()
            .any(|b| *b < 32 && !matches!(*b, 8 | 9 | 10 | 13))
            || bytes.contains(&127)
        {
            return Err("Esperá el regreso a input antes de enviar controles, prefijos o pegar con bracketed paste".into());
        }
        if p.bytes.len().saturating_add(bytes.len()) > LIMIT {
            return Err("Cola de regreso a input llena, 4 KiB; este evento no se envió".into());
        }
        p.bytes.extend_from_slice(bytes);
        Ok(true)
    }
    /// A stale/wrong-project ack never drains another context. Success returns
    /// exactly one batch; another acknowledgement is needed before releasing input.
    pub fn ack(
        &mut self,
        context: Context,
        token: u64,
        ready: bool,
        ok: bool,
        now: Instant,
    ) -> Option<Vec<u8>> {
        self.ack_with_failure_prefix(context, token, ready, ok, now, Vec::new)
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
        now: Instant,
        take_prefix: impl FnOnce() -> Vec<u8>,
    ) -> Option<Vec<u8>> {
        if !self.expects(context, token, ready) {
            return None;
        }
        if !ok && !self.awaiting_recovery_ack() {
            let prefix = take_prefix();
            if !prefix.is_empty() {
                let pending = self.pending.as_mut().expect("matching fence exists");
                debug_assert!(pending.raw_prefix.is_none());
                pending.raw_prefix = Some(Arc::new(prefix));
            }
        }
        let p = self.pending.as_mut()?;
        if p.token != token
            || p.context != context
            || p.waiting_ready != ready
            || p.halted_after_backend_failure
            || (!ready && p.retrying_enqueue)
        {
            return None;
        }
        if p.timed_out || p.rejected_waiting_ack {
            if ok && !ready {
                // A successful late write ack resolves only the earlier
                // in-flight batch; later buffered bytes were not sent.
                p.in_flight = None;
            }
            if ok && p.bytes.is_empty() && p.in_flight.is_none() && p.raw_prefix.is_none() {
                self.pending = None;
            } else {
                // Do not send buffered input after a timeout without a new
                // explicit user decision, even if the late ready ack succeeds.
                p.timed_out = false;
                p.rejected_waiting_ack = false;
                p.halted_after_backend_failure = true;
            }
            return None;
        }
        if !ok {
            if p.bytes.is_empty() && p.in_flight.is_none() && p.raw_prefix.is_none() {
                self.pending = None;
            } else {
                // Bytes in `bytes` have never left this fence. An earlier
                // batch may have run, so do not retry or concatenate it here.
                p.halted_after_backend_failure = true;
                p.retain_after_enqueue_failure = true;
            }
            return None;
        }
        if !ready {
            p.in_flight = None;
        }
        if p.bytes.is_empty() {
            self.pending = None;
            return None;
        }
        p.waiting_ready = false;
        p.since = now;
        let bytes = std::mem::take(&mut p.bytes);
        p.in_flight = Some(bytes.clone());
        Some(bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsent_input_during_reattach_is_retained_without_waiting_for_an_ack() {
        let context = Context {
            project: 2,
            attachment: 7,
        };
        let mut fence = Fence::default();
        fence
            .retain_before_backend(context, b"synthetic input".to_vec(), Instant::now())
            .unwrap();
        assert!(fence.active());
        assert!(fence.recovery_blocked());
        assert!(!fence.awaiting_recovery_ack());
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("synthetic input"));
        assert!(
            fence
                .retain_before_backend(context, b"new".to_vec(), Instant::now())
                .is_err()
        );
        assert!(fence.cancel_guarded(&guard));
        assert!(!fence.active());
        fence
            .retain_before_backend(context, b"\x1b[A".to_vec(), Instant::now())
            .unwrap();
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), None);
        assert_eq!(
            fence.prefix_hex_if_guarded(&guard).as_deref(),
            Some("1b5b41")
        );
        assert!(fence.cancel_guarded(&guard));
    }
    #[test]
    fn detached_ack_requires_the_original_attachment() {
        let now = Instant::now();
        let original = Context {
            project: 4,
            attachment: 77,
        };
        let other = Context {
            project: 4,
            attachment: 78,
        };
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(original, now);
        fence.push(original, b"preserved").unwrap();
        assert!(detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| Ok(())).unwrap());
        assert_eq!(
            deliveries.ack_for_context(other, token, true),
            DeliveryAck::Unknown
        );
        assert!(deliveries.busy(original.project));
        assert_eq!(
            deliveries.ack_for_context(original, token, true),
            DeliveryAck::Finished(None)
        );
        assert!(!deliveries.busy(original.project));
    }
    #[test]
    fn quit_override_cannot_discard_input_that_changed_after_prompt() {
        let now = Instant::now();
        let context = Context {
            project: 1,
            attachment: 5,
        };
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context, now);
        assert!(fence.retained_guard().is_none());
        fence.push(context, b"first").unwrap();
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| Err(
                "full".into()
            ))
            .is_err()
        );
        let stale = fence.retained_guard().unwrap();
        fence.push(context, b"-later").unwrap();
        assert!(!fence.cancel_guarded(&stale));
        let current = fence.retained_guard().unwrap();
        assert!(fence.cancel_guarded(&current));
        assert!(!fence.active());
        assert!(fence.ack(context, token, true, true, now).is_none());
    }
    #[test]
    fn quit_override_expires_when_pending_input_is_accepted() {
        let now = Instant::now();
        let context = Context {
            project: 1,
            attachment: 5,
        };
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context, now);
        fence.push(context, b"first").unwrap();
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| Err(
                "full".into()
            ))
            .is_err()
        );
        let stale = fence.retained_guard().unwrap();
        assert_eq!(
            fence.ack(context, token, true, true, now),
            Some(b"first".to_vec())
        );
        assert!(!fence.cancel_guarded(&stale));
        assert!(fence.active());
    }
    #[test]
    fn rejected_ack_enqueue_retries_in_order_without_inventing_an_ack() {
        let now = Instant::now();
        let context = Context {
            project: 1,
            attachment: 5,
        };
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context, now);
        fence.push(context, b"first").unwrap();
        let batch = fence.ack(context, token, true, true, now).unwrap();
        assert!(fence.retain_unsent(context, token, batch));
        fence.push(context, b"-second").unwrap();
        assert!(!fence.expired(now + TIMEOUT + Duration::from_secs(1)));
        assert!(!fence.expects(context, token, false));
        assert!(fence.ack(context, token, false, true, now).is_none());
        let (owner, id, retry) = fence.take_retry().unwrap();
        assert_eq!(
            (owner, id, retry.as_slice()),
            (context, token, b"first-second".as_slice())
        );
        assert!(fence.retain_unsent(owner, id, retry));
        let mut sent = Vec::new();
        assert_eq!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, bytes| {
                sent.extend_from_slice(bytes);
                Ok(())
            }),
            Ok(true)
        );
        assert_eq!(sent, b"first-second");
        assert_eq!(deliveries.ack(1, token, true), DeliveryAck::Finished(None));
        assert!(!deliveries.busy(1));
    }
    #[test]
    fn failed_detach_enqueue_preserves_buffer_and_ready_ack_path() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let context = Context {
            project: 1,
            attachment: 5,
        };
        let token = fence.begin(context, now);
        fence.push(context, b"kept").unwrap();
        let failure = detach_with_delivery(&mut fence, &mut deliveries, now, |owner, id, bytes| {
            assert_eq!((owner, id, bytes), (context, token, b"kept".as_slice()));
            Err("queue full".into())
        });
        assert_eq!(failure, Err("queue full".into()));
        assert!(fence.active());
        assert!(!deliveries.busy(1));
        assert!(!fence.expired(now + TIMEOUT + Duration::from_secs(1)));
        assert_eq!(
            fence.ack(context, token, true, true, now),
            Some(b"kept".to_vec())
        );
    }
    #[test]
    fn failed_detach_enqueue_preserves_inflight_ack_and_later_bytes() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let context = Context {
            project: 1,
            attachment: 5,
        };
        let token = fence.begin(context, now);
        fence.push(context, b"first").unwrap();
        assert_eq!(
            fence.ack(context, token, true, true, now),
            Some(b"first".to_vec())
        );
        fence.push(context, b"second").unwrap();
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| Err(
                "queue full".into()
            ))
            .is_err()
        );
        assert!(!deliveries.busy(1));
        assert!(fence.expects(context, token, false));
        assert_eq!(
            fence.ack(context, token, false, true, now),
            Some(b"second".to_vec())
        );
    }
    #[test]
    fn accepted_detach_tracks_once_and_releases_fence() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let context = Context {
            project: 1,
            attachment: 5,
        };
        let token = fence.begin(context, now);
        fence.push(context, b"kept").unwrap();
        let mut sent = Vec::new();
        assert_eq!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |owner, id, bytes| {
                assert_eq!((owner, id), (context, token));
                sent.extend_from_slice(bytes);
                Ok(())
            }),
            Ok(true)
        );
        assert_eq!(sent, b"kept");
        assert!(!fence.active());
        assert!(deliveries.busy(1));
        assert_eq!(deliveries.ack(1, token, true), DeliveryAck::Finished(None));
    }
    #[test]
    fn detached_failed_batches_remain_separate_and_are_never_automatically_replayed() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"first possibly sent").unwrap();
        assert_eq!(
            fence.ack(context(), token, true, true, now),
            Some(b"first possibly sent".to_vec())
        );
        fence.push(context(), b"second possibly sent").unwrap();
        let mut accepted = Vec::new();
        assert_eq!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, bytes| {
                accepted.extend_from_slice(bytes);
                Ok(())
            }),
            Ok(true)
        );
        assert_eq!(accepted, b"second possibly sent");
        assert_eq!(
            deliveries.ack(context().project, token, false),
            DeliveryAck::Failed
        );
        let stale = deliveries.failed_guard().unwrap();
        assert_eq!(
            deliveries.failed_text_if_guarded(stale),
            Some("first possibly sent")
        );
        assert!(!deliveries.quit_ready(Some(deliveries.generation())));
        assert_eq!(
            deliveries.ack(context().project, token, false),
            DeliveryAck::Failed
        );
        assert_eq!(deliveries.failed_text_if_guarded(stale), None);
        assert!(!deliveries.discard_failed_guarded(stale));
        let first = deliveries.failed_guard().unwrap();
        assert_eq!(
            deliveries.failed_text_if_guarded(first),
            Some("first possibly sent")
        );
        assert!(deliveries.discard_failed_guarded(first));
        let second = deliveries.failed_guard().unwrap();
        assert_eq!(
            deliveries.failed_text_if_guarded(second),
            Some("second possibly sent")
        );
        assert!(deliveries.discard_failed_guarded(second));
        assert!(!deliveries.any_busy());
        assert_eq!(
            deliveries.ack(context().project, token, false),
            DeliveryAck::Unknown
        );
    }
    #[test]
    fn full_delivery_retention_refuses_new_detach_before_sending() {
        let now = Instant::now();
        let mut deliveries = Deliveries::default();
        for i in 0..MAX_DELIVERY_BATCHES {
            assert!(deliveries.track(99, i as u64, 1, now));
        }
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"keep locally").unwrap();
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| {
                panic!("full retention must reject before enqueue")
            })
            .is_err()
        );
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("keep locally"));
        assert_eq!(deliveries.ack(99, 0, true), DeliveryAck::Pending);
        assert_eq!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, id, bytes| {
                assert_eq!((id, bytes), (token, b"keep locally".as_slice()));
                Ok(())
            }),
            Ok(true)
        );
    }
    #[test]
    fn terminal_delete_waits_for_resume_and_its_project_deliveries() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let context = Context {
            project: 1,
            attachment: 5,
        };
        let token = fence.begin(context, now);
        assert!(fence.push(context, b"queued").unwrap());
        assert!(terminal_delete_busy(
            &fence,
            &deliveries,
            1,
            false,
            false,
            false
        ));
        let pending = fence.detach().unwrap();
        assert_eq!(pending.bytes, b"queued");
        assert!(deliveries.track(1, token, 1, now));
        assert!(terminal_delete_busy(
            &fence,
            &deliveries,
            1,
            false,
            false,
            false
        ));
        assert!(!terminal_delete_busy(
            &fence,
            &deliveries,
            2,
            false,
            false,
            false
        ));
        assert_eq!(deliveries.ack(2, token, true), DeliveryAck::Unknown);
        assert!(terminal_delete_busy(
            &fence,
            &deliveries,
            1,
            false,
            false,
            false
        ));
        assert_eq!(deliveries.ack(1, token, true), DeliveryAck::Finished(None));
        assert!(!terminal_delete_busy(
            &fence,
            &deliveries,
            1,
            false,
            false,
            false
        ));
        for (search, rebind, open) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert!(terminal_delete_busy(
                &fence,
                &deliveries,
                1,
                search,
                rebind,
                open
            ));
        }
    }
    #[test]
    fn deferred_session_actions_wait_for_every_delivery_and_are_project_scoped() {
        let now = Instant::now();
        let mut deliveries = Deliveries::default();
        assert!(!deliveries.track(1, 10, 0, now));
        assert!(deliveries.track(1, 10, 2, now));
        assert!(!deliveries.track(1, 10, 1, now));
        assert!(deliveries.track(1, 11, 1, now));
        assert!(deliveries.track(2, 12, 1, now));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert!(deliveries.defer(1, DeferredAction::CloseWorkspace));
        assert!(!deliveries.defer(3, DeferredAction::Reconnect));
        assert_eq!(deliveries.ack(2, 10, true), DeliveryAck::Unknown);
        assert_eq!(deliveries.ack(1, 10, true), DeliveryAck::Pending);
        assert_eq!(deliveries.ack(1, 11, true), DeliveryAck::Pending);
        assert_eq!(
            deliveries.ack(1, 10, true),
            DeliveryAck::Finished(Some(DeferredAction::CloseWorkspace))
        );
        assert!(!deliveries.busy(1));
        assert!(deliveries.busy(2));
        assert_eq!(deliveries.ack(2, 12, true), DeliveryAck::Finished(None));
    }
    #[test]
    fn failure_and_timeout_cancel_deferred_actions_without_assuming_non_delivery() {
        let now = Instant::now();
        let mut deliveries = Deliveries::default();
        assert!(deliveries.track(1, 20, 2, now));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert_eq!(deliveries.ack(1, 20, false), DeliveryAck::Failed);
        assert!(deliveries.busy(1));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert_eq!(deliveries.ack(1, 20, true), DeliveryAck::Pending);
        let failed = deliveries.failed_guard().unwrap();
        assert_eq!(deliveries.failed_text_if_guarded(failed), None);
        assert!(deliveries.discard_failed_guarded(failed));
        assert!(!deliveries.busy(1));
        assert!(deliveries.track(1, 21, 1, now));
        assert!(deliveries.defer(1, DeferredAction::CloseWorkspace));
        assert!(
            deliveries
                .warn_slow(now + TIMEOUT - Duration::from_nanos(1))
                .is_empty()
        );
        assert_eq!(deliveries.warn_slow(now + TIMEOUT), vec![1]);
        assert!(deliveries.warn_slow(now + TIMEOUT).is_empty());
        assert!(deliveries.busy(1));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert_eq!(deliveries.ack(1, 21, true), DeliveryAck::Finished(None));
        assert!(!deliveries.busy(1));
    }
    #[test]
    fn quit_override_covers_only_the_deliveries_the_user_was_warned_about() {
        let now = Instant::now();
        let mut deliveries = Deliveries::default();
        assert!(deliveries.quit_ready(None));
        assert!(deliveries.track(1, 10, 1, now));
        assert!(deliveries.defer(1, DeferredAction::Reconnect));
        assert!(!deliveries.quit_ready(None));
        let approved = deliveries.generation();
        assert!(deliveries.quit_ready(Some(approved)));
        deliveries.cancel_deferred();
        assert!(deliveries.track(2, 20, 1, now));
        assert!(!deliveries.quit_ready(Some(approved)));
        assert_eq!(deliveries.ack(1, 10, true), DeliveryAck::Finished(None));
        assert!(!deliveries.quit_ready(Some(approved)));
        assert_eq!(deliveries.ack(2, 20, true), DeliveryAck::Finished(None));
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
    fn context() -> Context {
        Context {
            project: 1,
            attachment: 5,
        }
    }
    #[test]
    fn rejected_control_waits_for_ack_without_sending_any_later_batch() {
        let now = Instant::now();
        let c = context();
        let mut fence = Fence::default();
        let token = fence.begin(c, now);
        fence.push(c, b"before").unwrap();
        assert!(fence.push(c, b"\x1bx").is_err());
        fence.retain_rejected(b"\x1bx".to_vec()).unwrap();
        assert!(fence.recovery_blocked());
        assert!(fence.awaiting_recovery_ack());
        assert!(fence.push(c, b"after").is_err());
        assert!(fence.retain_rejected(b"second".to_vec()).is_err());
        assert!(fence.ack(c, token + 1, true, true, now).is_none());
        assert!(fence.ack(c, token, true, true, now).is_none());
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("before"));
        assert_eq!(fence.prefix_hex_if_guarded(&guard).as_deref(), Some("1b78"));
        assert!(fence.recovery_blocked());
        let mut deliveries = Deliveries::default();
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| panic!(
                "must not send"
            ))
            .is_err()
        );
        assert!(fence.cancel_guarded(&guard));
    }
    #[test]
    fn rejected_control_keeps_unsent_after_inflight_success_or_failure() {
        let now = Instant::now();
        let c = context();
        for ok in [true, false] {
            let mut fence = Fence::default();
            let token = fence.begin(c, now);
            fence.push(c, b"first").unwrap();
            assert_eq!(
                fence.ack(c, token, true, true, now),
                Some(b"first".to_vec())
            );
            fence.push(c, b"later").unwrap();
            fence.retain_rejected(b"\x1c".to_vec()).unwrap();
            assert!(fence.ack(c, token, false, ok, now).is_none());
            let guard = fence.retained_guard().unwrap();
            assert_eq!(fence.text_if_guarded(&guard), Some("later"));
            assert_eq!(fence.prefix_hex_if_guarded(&guard).as_deref(), Some("1c"));
            assert_eq!(
                fence.uncertain_text_if_guarded(&guard),
                (!ok).then_some("first")
            );
        }
    }
    #[test]
    fn rejected_control_during_local_enqueue_retry_does_not_wait_for_impossible_ack() {
        let now = Instant::now();
        let c = context();
        let mut fence = Fence::default();
        let token = fence.begin(c, now);
        fence.push(c, b"first").unwrap();
        let bytes = fence.ack(c, token, true, true, now).unwrap();
        assert!(fence.retain_unsent(c, token, bytes));
        fence.retain_rejected(b"\x1b".to_vec()).unwrap();
        assert!(!fence.awaiting_recovery_ack());
        assert!(fence.recovery_blocked());
        assert!(fence.take_retry().is_none());
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("first"));
        assert_eq!(fence.prefix_hex_if_guarded(&guard).as_deref(), Some("1b"));
    }
    #[test]
    fn immediate_text_waits_for_ready_and_delivery_then_preserves_order() {
        let now = Instant::now();
        let mut f = Fence::default();
        let token = f.begin(context(), now);
        assert_eq!(f.push(context(), b"first\r"), Ok(true));
        assert!(f.ack(context(), token, false, true, now).is_none());
        assert_eq!(
            f.ack(context(), token, true, true, now),
            Some(b"first\r".to_vec())
        );
        assert!(f.active());
        f.push(context(), "ñ second".as_bytes()).unwrap();
        assert_eq!(
            f.ack(context(), token, false, true, now),
            Some("ñ second".as_bytes().to_vec())
        );
        assert!(f.active());
        assert_eq!(f.ack(context(), token, false, true, now), None);
        assert!(!f.active());
        assert_eq!(f.push(context(), b"direct"), Ok(false));
    }
    #[test]
    fn timeout_before_ready_keeps_unsent_input_and_late_ready_does_not_send_it() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"queued").unwrap();
        assert_eq!(
            fence.mark_expired(now + TIMEOUT - Duration::from_nanos(1)),
            ResumeTimeout::NotExpired
        );
        assert_eq!(fence.mark_expired(now + TIMEOUT), ResumeTimeout::Retained);
        let stale = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&stale), Some("queued"));
        assert!(fence.push(context(), b"new").is_err());
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| panic!(
                "must not enqueue after timeout"
            ))
            .is_err()
        );
        assert_eq!(fence.ack(context(), token + 1, true, true, now), None);
        assert!(fence.timed_out());
        assert_eq!(fence.ack(context(), token, true, true, now), None);
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
        fence.push(context(), b"sent earlier").unwrap();
        assert_eq!(
            fence.ack(context(), token, true, true, now),
            Some(b"sent earlier".to_vec())
        );
        fence.push(context(), b"not sent later").unwrap();
        assert_eq!(fence.mark_expired(now + TIMEOUT), ResumeTimeout::Retained);
        let old = fence.retained_guard().unwrap();
        assert_eq!(fence.uncertain_text_if_guarded(&old), Some("sent earlier"));
        assert_eq!(
            fence.ack(context(), token, false, true, now + TIMEOUT),
            None
        );
        assert!(!fence.cancel_guarded(&old));
        let new = fence.retained_guard().unwrap();
        assert_eq!(fence.uncertain_text_if_guarded(&new), None);
        assert_eq!(fence.text_if_guarded(&new), Some("not sent later"));
        assert!(fence.cancel_guarded(&new));
    }
    #[test]
    fn timeout_with_no_input_clears_and_late_error_retains_uncertain_input() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let old = fence.begin(context(), now);
        assert_eq!(fence.mark_expired(now + TIMEOUT), ResumeTimeout::NoInput);
        assert!(!fence.active());
        assert_eq!(fence.ack(context(), old, true, true, now), None);
        let token = fence.begin(context(), now);
        fence.push(context(), b"possibly sent").unwrap();
        assert_eq!(
            fence.ack(context(), token, true, true, now),
            Some(b"possibly sent".to_vec())
        );
        assert_eq!(fence.mark_expired(now + TIMEOUT), ResumeTimeout::Retained);
        assert_eq!(
            fence.ack(context(), token, false, false, now + TIMEOUT),
            None
        );
        let guard = fence.retained_guard().unwrap();
        assert_eq!(
            fence.uncertain_text_if_guarded(&guard),
            Some("possibly sent")
        );
        assert!(fence.cancel_guarded(&guard));
    }
    #[test]
    fn cancel_error_timeout_and_other_project_or_attachment_never_drain_bytes() {
        let now = Instant::now();
        let mut f = Fence::default();
        let old = f.begin(context(), now);
        f.push(context(), b"old").unwrap();
        assert!(f.cancel());
        let token = f.begin(context(), now);
        f.push(context(), b"new").unwrap();
        for (ctx, id) in [
            (context(), old),
            (
                Context {
                    project: 2,
                    ..context()
                },
                token,
            ),
            (
                Context {
                    attachment: 6,
                    ..context()
                },
                token,
            ),
        ] {
            assert!(f.ack(ctx, id, true, true, now).is_none());
        }
        assert!(!f.expired(now));
        assert!(f.expired(now + TIMEOUT));
        assert!(f.ack(context(), token, true, false, now).is_none());
        let guard = f.retained_guard().expect("unsent input is retained");
        assert_eq!(f.text_if_guarded(&guard), Some("new"));
        assert!(!f.expects(context(), token, true));
        assert!(!f.expired(now + TIMEOUT + TIMEOUT));
        assert!(f.cancel_guarded(&guard));
        assert!(!f.active());
    }
    #[test]
    fn wrong_context_keeps_queued_bytes_for_the_original_terminal() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let original = context();
        let token = fence.begin(original, now);
        fence.push(original, b"pending").unwrap();
        let other = Context {
            project: original.project + 1,
            ..original
        };
        assert!(fence.push(other, b"must-not-be-sent").is_err());
        assert!(fence.retained_guard().is_some());
        assert!(!fence.expired(now + TIMEOUT));
        assert_eq!(
            fence.ack(original, token, true, true, now),
            Some(b"pending".to_vec())
        );
    }
    #[test]
    fn copying_requires_current_snapshot_and_valid_utf8() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let mut deliveries = Deliveries::default();
        fence.begin(context(), now);
        fence.push(context(), b"before").unwrap();
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| Err(
                "full".into()
            ))
            .is_err()
        );
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
        assert_eq!(fence.ack(context(), token, true, false, now), None);
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("not sent"));
        assert!(fence.push(context(), b"new event").is_err());
        assert!(
            detach_with_delivery(&mut fence, &mut deliveries, now, |_, _, _| {
                panic!("a failed transition must not send or retarget the bytes")
            })
            .is_err()
        );
        assert_eq!(fence.ack(context(), token, true, true, now), None);
        assert_eq!(fence.text_if_guarded(&guard), Some("not sent"));
        assert!(fence.cancel_guarded(&guard));
    }
    #[test]
    fn failed_write_retains_only_later_unsent_bytes_and_never_replays_prior_batch() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"possibly sent").unwrap();
        assert_eq!(
            fence.ack(context(), token, true, true, now),
            Some(b"possibly sent".to_vec())
        );
        fence.push(context(), b"definitely unsent").unwrap();
        assert_eq!(fence.ack(context(), token, false, false, now), None);
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), Some("definitely unsent"));
        assert_eq!(
            fence.uncertain_text_if_guarded(&guard),
            Some("possibly sent")
        );
        assert!(fence.take_retry().is_none());
        assert!(fence.cancel_guarded(&guard));
    }
    #[test]
    fn failed_write_without_later_input_retains_the_uncertain_batch() {
        let now = Instant::now();
        let mut fence = Fence::default();
        let token = fence.begin(context(), now);
        fence.push(context(), b"possibly executed").unwrap();
        assert_eq!(
            fence.ack(context(), token, true, true, now),
            Some(b"possibly executed".to_vec())
        );
        assert_eq!(fence.ack(context(), token, false, false, now), None);
        let guard = fence.retained_guard().unwrap();
        assert_eq!(fence.text_if_guarded(&guard), None);
        assert_eq!(
            fence.uncertain_text_if_guarded(&guard),
            Some("possibly executed")
        );
        assert!(fence.cancel_guarded(&guard));
    }
    #[test]
    fn queue_limits_and_controls_reject_whole_events_and_keep_prior_text() {
        let now = Instant::now();
        let mut f = Fence::default();
        let token = f.begin(context(), now);
        let text = vec![b'x'; LIMIT];
        f.push(context(), &text).unwrap();
        assert!(f.push(context(), b"extra").is_err());
        assert!(f.push(context(), b"\x01").is_err());
        assert!(f.push(context(), b"\x1b[200~paste\x1b[201~").is_err());
        assert_eq!(f.ack(context(), token, true, true, now), Some(text));
        assert!(
            f.push(
                Context {
                    project: 2,
                    ..context()
                },
                b"wrong"
            )
            .is_err()
        );
        assert!(f.active());
        assert_eq!(f.ack(context(), token, false, true, now), None);
        assert!(!f.active());
    }
    #[test]
    fn detached_batches_keep_original_target_token_and_delivery_state() {
        let now = Instant::now();
        let mut f = Fence::default();
        let token = f.begin(context(), now);
        f.push(context(), b"first\r").unwrap();
        assert_eq!(
            f.detach(),
            Some(Detached {
                context: context(),
                token,
                bytes: b"first\r".to_vec(),
                in_flight: false,
            })
        );
        assert!(!f.active());
        assert!(f.ack(context(), token, true, true, now).is_none());

        let token = f.begin(context(), now);
        f.push(context(), b"first").unwrap();
        assert_eq!(
            f.ack(context(), token, true, true, now),
            Some(b"first".to_vec())
        );
        f.push(context(), b"\r").unwrap();
        assert_eq!(
            f.detach(),
            Some(Detached {
                context: context(),
                token,
                bytes: b"\r".to_vec(),
                in_flight: true,
            })
        );
        assert!(!f.active());
    }
}
