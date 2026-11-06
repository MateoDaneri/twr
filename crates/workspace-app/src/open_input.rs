//! Ordered, recoverable input held while an editor buffer is loading.
//! This ledger does not read files, dispatch events, or log input contents.
use std::{collections::VecDeque, ops::Range, path::PathBuf, sync::Arc};

#[derive(Clone, PartialEq, Eq)]
pub struct Context {
    pub workspace: usize,
    pub pane: u64,
    pub attachment: u64,
    pub generation: u64,
    pub path: PathBuf,
}

/// Native text ranges are UTF-16, exactly as delivered by the input method.
#[derive(Clone, PartialEq, Eq)]
pub enum Event<K> {
    Key(K),
    Commit {
        range: Option<Range<usize>>,
        text: String,
    },
    Paste {
        text: String,
    },
    Mark {
        range: Option<Range<usize>>,
        text: String,
        selected: Option<Range<usize>>,
    },
    Unmark,
}

/// Replacement ranges obtained before the target buffer loaded cannot be
/// interpreted safely against its eventual text context.
pub fn requires_loaded_context<K>(event: &Event<K>) -> bool {
    matches!(
        event,
        Event::Commit { range: Some(_), .. } | Event::Mark { range: Some(_), .. }
    )
}

struct Entry<K> {
    id: u64,
    bytes: usize,
    event: Event<K>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Recording,
    Ready,
    Recover,
    Uncertain,
}

/// The caller retains an event rejected by `push`, and must choose its own
/// backpressure/recovery; rejection never means the event was delivered.
pub struct Queue<K> {
    identity: Arc<()>,
    context: Context,
    state: State,
    entries: VecDeque<Entry<K>>,
    bytes: usize,
    next: Option<u64>,
    in_flight: Option<u64>,
    edit_in_flight: Option<u64>,
    max_events: usize,
    max_bytes: usize,
}

impl<K> Queue<K> {
    pub fn new(context: Context, max_events: usize, max_bytes: usize) -> Self {
        Self {
            identity: Arc::new(()),
            context,
            state: State::Recording,
            entries: VecDeque::new(),
            bytes: 0,
            next: Some(1),
            in_flight: None,
            edit_in_flight: None,
            max_events,
            max_bytes,
        }
    }
    /// Identifies this pending operation across guarded path resolution and
    /// retry, without exposing its path or allowing a later queue to reuse it.
    pub fn identity(&self) -> Arc<()> {
        self.identity.clone()
    }
    pub fn state(&self) -> State {
        self.state
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Identifies the event currently owned by the dispatcher, if any.
    pub fn in_flight_id(&self) -> Option<u64> {
        self.in_flight
    }
    pub fn edit_in_flight(&self, id: u64) -> bool {
        self.edit_in_flight == Some(id) && self.in_flight == Some(id)
    }
    /// Intended for an explicitly requested recovery view; callers must not
    /// log event contents or assume an in-flight event was not executed.
    pub fn events(&self) -> impl Iterator<Item = &Event<K>> {
        self.entries.iter().map(|entry| &entry.event)
    }
    pub fn push(&mut self, event: Event<K>, key_bytes: usize) -> Result<(), Event<K>> {
        let bytes = match &event {
            Event::Key(_) => key_bytes,
            Event::Commit { text, .. } | Event::Mark { text, .. } | Event::Paste { text } => {
                text.len()
            }
            Event::Unmark => 0,
        };
        let Some(id) = self.next else {
            return Err(event);
        };
        let Some(total) = self.bytes.checked_add(bytes) else {
            return Err(event);
        };
        if !matches!(self.state, State::Recording | State::Ready)
            || self.entries.len() >= self.max_events
            || total > self.max_bytes
        {
            return Err(event);
        }
        self.next = id.checked_add(1);
        self.bytes = total;
        self.entries.push_back(Entry { id, bytes, event });
        Ok(())
    }
    /// The caller must also verify a focus epoch that cannot be restored by
    /// switching away and back; `invalidate` records such a change permanently.
    pub fn ready(&mut self, current: &Context, focus_current: bool) -> bool {
        if self.state != State::Recording || !focus_current || current != &self.context {
            self.invalidate();
            return false;
        }
        self.state = State::Ready;
        true
    }
    pub fn invalidate(&mut self) {
        self.state = if self.state == State::Uncertain || self.in_flight.is_some() {
            State::Uncertain
        } else {
            State::Recover
        };
    }
    /// Earlier pane work did not complete unambiguously. Even though these
    /// events were not dispatched, replaying them before that work would
    /// change their meaning, so require explicit review.
    pub fn block_after_predecessor(&mut self) {
        self.state = State::Uncertain;
    }

    /// The caller must validate the original focus, newly created pane and
    /// file identity before transferring queued input after its own split.
    pub fn transfer_after_split(
        &mut self,
        expected: &Context,
        next: Context,
        new_file: bool,
    ) -> bool {
        if self.state != State::Ready
            || self.in_flight.is_some()
            || self.edit_in_flight.is_some()
            || &self.context != expected
            || next.workspace != expected.workspace
            || next.pane == expected.pane
            || next.attachment != 0
            || expected.attachment != 0
            || if new_file {
                next.path == expected.path || next.generation <= expected.generation
            } else {
                next.path != expected.path || next.generation != expected.generation
            }
        {
            return false;
        }
        self.context = next;
        true
    }

    /// A replayed split key stays in flight until the caller confirms the
    /// new pane. Acknowledge exactly that event and transfer its successors.
    pub fn acknowledge_split_and_transfer(
        &mut self,
        id: u64,
        expected: &Context,
        next: Context,
        new_file: bool,
    ) -> bool {
        if self.state != State::Ready
            || self.in_flight != Some(id)
            || self.edit_in_flight.is_some()
            || self.entries.front().is_none_or(|entry| entry.id != id)
            || &self.context != expected
            || next.workspace != expected.workspace
            || next.pane == expected.pane
            || next.attachment != 0
            || if new_file {
                next.path == expected.path || next.generation <= expected.generation
            } else {
                next.path != expected.path || next.generation != expected.generation
            }
        {
            return false;
        }
        if !self.acknowledge(id) {
            return false;
        }
        self.context = next;
        true
    }

    /// A split-with-path has created its pane but has not opened the path.
    /// Move the still-in-flight event to that pane without acknowledging it;
    /// the subsequent guarded edit owns final acknowledgement.
    pub fn rebind_split_for_edit(&mut self, id: u64, expected: &Context, next: Context) -> bool {
        if self.state != State::Ready
            || self.in_flight != Some(id)
            || self.edit_in_flight.is_some()
            || self.entries.front().is_none_or(|entry| entry.id != id)
            || &self.context != expected
            || next.workspace != expected.workspace
            || next.pane == expected.pane
            || next.attachment != 0
            || next.generation != expected.generation
            || next.path != expected.path
        {
            return false;
        }
        self.context = next;
        true
    }

    /// The caller validates the unchanged sidebar origin and the selected
    /// editing pane before transferring a still-recording file-open queue.
    pub fn transfer_after_sidebar_open(&mut self, expected: &Context, next: Context) -> bool {
        if self.state != State::Recording
            || self.in_flight.is_some()
            || self.edit_in_flight.is_some()
            || &self.context != expected
            || next.workspace != expected.workspace
            || next.attachment != 0
            || expected.attachment != 0
            || next.generation != expected.generation
            || next.path != expected.path
        {
            return false;
        }
        self.context = next;
        true
    }
    /// Rebind an unresolved :edit request after its own guarded resolver has
    /// returned a canonical destination. Never use this for arbitrary opens.
    pub fn resolved_path(
        &mut self,
        expected: &Context,
        path: PathBuf,
        focus_current: bool,
    ) -> bool {
        if self.state != State::Recording
            || self.in_flight != self.edit_in_flight
            || !focus_current
            || &self.context != expected
        {
            return false;
        }
        self.context.path = path;
        true
    }
    /// Transfer the same pending operation from path resolution to its read.
    pub fn advance_read(&mut self, expected: &Context, next: Context, focus_current: bool) -> bool {
        if self.state != State::Recording
            || self.in_flight != self.edit_in_flight
            || !focus_current
            || &self.context != expected
            || next.workspace != expected.workspace
            || next.pane != expected.pane
            || next.attachment != expected.attachment
            || next.path != expected.path
            || next.generation <= expected.generation
        {
            return false;
        }
        self.context = next;
        true
    }
    /// Only an explicit retry may rebind retained, never-dispatched input to a
    /// new read generation. It must keep the same file, pane and attachment;
    /// an uncertain in-flight event can never be retried automatically.
    pub fn retry(&mut self, current: Context, focus_current: bool) -> bool {
        if self.state != State::Recover
            || self.in_flight.is_some()
            || !focus_current
            || current.workspace != self.context.workspace
            || current.pane != self.context.pane
            || current.attachment != self.context.attachment
            || current.path != self.context.path
            || current.generation <= self.context.generation
        {
            return false;
        }
        self.context = current;
        self.state = State::Recording;
        true
    }
    /// Inspect one event while retaining ownership. At most one event may be
    /// in flight; a changed destination cannot retarget the remaining input.
    pub fn begin(&mut self, current: &Context, focus_current: bool) -> Option<(u64, &Event<K>)> {
        if self.state != State::Ready || self.in_flight.is_some() {
            return None;
        }
        if !focus_current || current != &self.context {
            self.invalidate();
            return None;
        }
        let entry = self.entries.front()?;
        self.in_flight = Some(entry.id);
        Some((entry.id, &entry.event))
    }
    pub fn acknowledge(&mut self, id: u64) -> bool {
        if self.state != State::Ready
            || self.in_flight != Some(id)
            || self.entries.front().is_none_or(|entry| entry.id != id)
        {
            return false;
        }
        let entry = self.entries.pop_front().expect("front checked");
        self.bytes -= entry.bytes;
        self.in_flight = None;
        true
    }
    /// Keep the :edit event in flight while its own guarded resolver and read
    /// transfer this queue to another file in the same pane.
    pub fn retarget_edit(&mut self, id: u64, expected: &Context, next: Context) -> bool {
        if self.state != State::Ready
            || self.in_flight != Some(id)
            || self.edit_in_flight.is_some()
            || self.entries.front().is_none_or(|entry| entry.id != id)
            || &self.context != expected
            || next.workspace != expected.workspace
            || next.pane != expected.pane
            || next.attachment != 0
            || next.generation != expected.generation.wrapping_add(1)
        {
            return false;
        }
        self.context = next;
        self.edit_in_flight = Some(id);
        self.state = State::Recording;
        true
    }
    /// Acknowledge only after the new buffer and focus have been validated.
    pub fn acknowledge_edit(&mut self) -> bool {
        let Some(id) = self.edit_in_flight else {
            return true;
        };
        if !self.acknowledge(id) {
            return false;
        }
        self.edit_in_flight = None;
        true
    }
    /// Commit a jump's in-flight event and transfer this same ordered queue
    /// to its confirmed destination in the same pane. The owner must first
    /// validate focus, editor identity, and the destination that was loaded.
    pub fn acknowledge_jump(&mut self, id: Option<u64>, expected: &Context, next: Context) -> bool {
        if &self.context != expected
            || next.workspace != expected.workspace
            || next.pane != expected.pane
            || next.attachment != expected.attachment
            || next.generation != expected.generation.wrapping_add(1)
        {
            return false;
        }
        if let Some(id) = id {
            if !self.acknowledge(id) {
                return false;
            }
        } else if self.state != State::Ready || self.in_flight.is_some() {
            return false;
        }
        self.context = next;
        true
    }
    /// The dispatcher has proved it did not run this event. Preserve it for
    /// user-directed recovery without labeling the outcome uncertain.
    pub fn not_dispatched(&mut self, id: u64) -> bool {
        if self.state != State::Ready || self.in_flight != Some(id) {
            return false;
        }
        self.in_flight = None;
        self.state = State::Recover;
        true
    }
    /// Delivery might have happened. Keep the front and block replay; the
    /// caller must not automatically execute an uncertain command twice.
    pub fn uncertain(&mut self, id: u64) -> bool {
        if self.in_flight != Some(id) {
            return false;
        }
        self.state = State::Uncertain;
        true
    }
    /// The caller must explicitly resolve or retain these events. An uncertain
    /// front may already have run and must be presented separately to the user.
    pub fn into_recovery(self) -> (Option<u64>, Vec<(u64, Event<K>)>) {
        (
            self.in_flight,
            self.entries
                .into_iter()
                .map(|entry| (entry.id, entry.event))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Editor, Mode};

    fn context() -> Context {
        Context {
            workspace: 3,
            pane: 4,
            attachment: 5,
            generation: 6,
            path: "/synthetic/a.txt".into(),
        }
    }

    #[test]
    fn edit_resolution_transfers_only_the_guarded_operation_and_preserves_input() {
        let original = context();
        let mut queue = Queue::new(original.clone(), 8, 128);
        assert!(queue.push(Event::Key("i"), 1).is_ok());
        let mut wrong = original.clone();
        wrong.pane += 1;
        assert!(!queue.resolved_path(&wrong, "/resolved/file".into(), true));
        assert!(!queue.resolved_path(&original, "/resolved/file".into(), false));
        assert!(queue.resolved_path(&original, "/resolved/file".into(), true));
        let mut resolved = original.clone();
        resolved.path = "/resolved/file".into();
        let mut read = resolved.clone();
        read.generation += 1;
        let mut wrong_read = read.clone();
        wrong_read.attachment += 1;
        assert!(!queue.advance_read(&resolved, wrong_read, true));
        assert!(!queue.advance_read(&resolved, read.clone(), false));
        assert!(queue.advance_read(&resolved, read.clone(), true));
        assert!(
            queue
                .push(
                    Event::Commit {
                        range: None,
                        text: "ñ".into()
                    },
                    0
                )
                .is_ok()
        );
        assert!(queue.ready(&read, true));
        let first = queue.begin(&read, true).unwrap().0;
        assert!(queue.acknowledge(first));
        assert!(
            matches!(queue.begin(&read, true).unwrap().1, Event::Commit { text, .. } if text == "ñ")
        );
        assert!(!queue.resolved_path(&read, "/other".into(), true));
    }

    #[test]
    fn preserves_order_and_native_utf16_composition_through_real_editor() {
        let context = context();
        let mut queue = Queue::new(context.clone(), 8, 128);
        assert!(queue.push(Event::Key("i"), 1).is_ok());
        assert!(
            queue
                .push(
                    Event::Mark {
                        range: None,
                        text: "´".into(),
                        selected: Some(1..1)
                    },
                    0
                )
                .is_ok()
        );
        assert!(
            queue
                .push(
                    Event::Commit {
                        range: None,
                        text: "é".into()
                    },
                    0
                )
                .is_ok()
        );
        assert!(queue.push(Event::Key("<Esc>"), 5).is_ok());
        assert!(queue.push(Event::Key("j"), 1).is_ok());
        assert!(queue.ready(&context, true));
        let mut editor = Editor::new("alpha\nbeta".into());
        while let Some((id, event)) = queue
            .begin(&context, true)
            .map(|(id, event)| (id, event.clone()))
        {
            match event {
                Event::Key(key) => {
                    editor.key(key);
                }
                Event::Mark {
                    range,
                    text,
                    selected,
                } => editor.mark_input(range, &text, selected).unwrap(),
                Event::Commit { range, text } => editor.commit_input(range, &text).unwrap(),
                Event::Paste { text } => editor.paste(&text),
                Event::Unmark => editor.unmark_input().unwrap(),
            }
            assert!(queue.acknowledge(id));
        }
        assert_eq!(editor.contents(), "éalpha\nbeta");
        assert_eq!(editor.mode, Mode::Normal);
        assert_eq!(editor.position().0, 1);
        assert!(queue.is_empty());
    }

    #[test]
    fn destination_changes_and_uncertain_delivery_retain_every_event() {
        let context = context();
        let mut queue = Queue::new(context.clone(), 3, 16);
        assert!(queue.push(Event::Key("i"), 1).is_ok());
        assert!(
            queue
                .push(
                    Event::Commit {
                        range: None,
                        text: "😀".into()
                    },
                    0
                )
                .is_ok()
        );
        assert!(queue.ready(&context, true));
        let id = queue.begin(&context, true).unwrap().0;
        assert!(queue.begin(&context, true).is_none());
        queue.invalidate();
        assert_eq!(queue.state(), State::Uncertain);
        assert!(!queue.acknowledge(id));
        let (uncertain, retained) = queue.into_recovery();
        assert_eq!(uncertain, Some(id));
        assert_eq!(retained.len(), 2);
        let mut moved = Queue::new(context.clone(), 2, 8);
        assert!(moved.push(Event::Key("a"), 1).is_ok());
        let mut other = context.clone();
        other.pane += 1;
        assert!(!moved.ready(&other, true));
        assert_eq!(moved.state(), State::Recover);
        assert!(!moved.ready(&context, true));
        assert_eq!(moved.into_recovery().1.len(), 1);
    }

    #[test]
    fn overflow_returns_the_rejected_event_and_rejects_duplicate_ack() {
        let context = context();
        let mut queue = Queue::new(context.clone(), 2, 3);
        assert!(queue.push(Event::Key("x"), 1).is_ok());
        assert!(
            queue
                .push(
                    Event::Commit {
                        range: Some(0..0),
                        text: "é".into()
                    },
                    0
                )
                .is_ok()
        );
        let rejected = queue.push(Event::Key("z"), 1);
        assert!(matches!(rejected, Err(Event::Key("z"))));
        assert!(queue.ready(&context, true));
        let id = queue.begin(&context, true).unwrap().0;
        assert!(!queue.acknowledge(id + 1));
        assert!(queue.acknowledge(id));
        assert!(!queue.acknowledge(id));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn input_arriving_during_dispatch_stays_behind_the_in_flight_event() {
        let context = context();
        let mut queue = Queue::new(context.clone(), 3, 16);
        assert!(queue.push(Event::Key("i"), 1).is_ok());
        assert!(queue.ready(&context, true));
        let first = queue.begin(&context, true).unwrap().0;
        assert!(
            queue
                .push(
                    Event::Commit {
                        range: None,
                        text: "á".into()
                    },
                    0
                )
                .is_ok()
        );
        assert!(queue.begin(&context, true).is_none());
        assert!(queue.acknowledge(first));
        let (second, event) = queue.begin(&context, true).unwrap();
        assert!(matches!(event, Event::Commit { text, .. } if text == "á"));
        assert_ne!(first, second);
        assert!(queue.acknowledge(second));
        assert!(queue.is_empty());
    }

    #[test]
    fn a_proven_undispatched_composition_stays_available_for_explicit_retry() {
        let context = context();
        let mut queue = Queue::<&str>::new(context.clone(), 3, 32);
        assert!(
            queue
                .push(
                    Event::Mark {
                        range: Some(0..1),
                        text: "é".into(),
                        selected: Some(1..1)
                    },
                    0
                )
                .is_ok()
        );
        assert!(queue.ready(&context, true));
        let id = queue.begin(&context, true).unwrap().0;
        assert!(queue.not_dispatched(id));
        assert_eq!(queue.state(), State::Recover);
        assert!(!queue.acknowledge(id));
        assert!(
            matches!(queue.events().next(), Some(Event::Mark { range: Some(range), text, .. }) if *range == (0..1) && text == "é")
        );
        let mut next = context;
        next.generation += 1;
        assert!(queue.retry(next.clone(), true));
        assert!(queue.ready(&next, true));
        assert_eq!(queue.begin(&next, true).unwrap().0, id);
    }

    #[test]
    fn only_absolute_replacement_ranges_require_missing_text_context() {
        assert!(!requires_loaded_context(&Event::Key("i")));
        assert!(!requires_loaded_context(&Event::<&str>::Mark {
            range: None,
            text: "é".into(),
            selected: Some(1..1)
        }));
        assert!(requires_loaded_context(&Event::<&str>::Mark {
            range: Some(1..2),
            text: "é".into(),
            selected: None
        }));
        assert!(requires_loaded_context(&Event::<&str>::Commit {
            range: Some(0..0),
            text: "á".into()
        }));
        assert!(!requires_loaded_context(&Event::<&str>::Commit {
            range: None,
            text: "😀".into()
        }));
    }

    #[test]
    fn paste_holds_the_original_clipboard_contents_until_dispatch() {
        let context = context();
        let mut clipboard = String::from("primero ñ");
        let mut queue = Queue::<&str>::new(context.clone(), 3, 128);
        assert!(queue.push(Event::Key("i"), 1).is_ok());
        assert!(
            queue
                .push(
                    Event::Paste {
                        text: clipboard.clone()
                    },
                    0
                )
                .is_ok()
        );
        clipboard.replace_range(.., "cambiado");
        assert!(queue.ready(&context, true));
        let mut editor = Editor::new(String::new());
        while let Some((id, event)) = queue
            .begin(&context, true)
            .map(|(id, event)| (id, event.clone()))
        {
            match event {
                Event::Key(key) => {
                    editor.key(key);
                }
                Event::Paste { text } => editor.paste(&text),
                _ => panic!("unexpected event"),
            }
            assert!(queue.acknowledge(id));
        }
        assert_eq!(clipboard, "cambiado");
        assert_eq!(editor.contents(), "primero ñ");
    }

    #[test]
    fn retry_preserves_input_and_requires_original_owner_new_generation() {
        let context = context();
        let mut queue = Queue::new(context.clone(), 3, 16);
        assert!(queue.push(Event::Key("i"), 1).is_ok());
        assert!(
            queue
                .push(
                    Event::Commit {
                        range: None,
                        text: "ñ".into()
                    },
                    0
                )
                .is_ok()
        );
        queue.invalidate();
        assert!(!queue.retry(context.clone(), true));
        let mut older = context.clone();
        older.generation -= 1;
        assert!(!queue.retry(older, true));
        let mut retry = context.clone();
        retry.generation += 1;
        assert!(!queue.retry(retry.clone(), false));
        let mut wrong = retry.clone();
        wrong.attachment += 1;
        assert!(!queue.retry(wrong, true));
        assert!(queue.retry(retry.clone(), true));
        assert!(queue.ready(&retry, true));
        let (id, _) = queue.begin(&retry, true).unwrap();
        assert!(queue.acknowledge(id));
        let (id, _) = queue.begin(&retry, true).unwrap();
        assert!(queue.uncertain(id));
        retry.generation += 1;
        assert!(!queue.retry(retry, true));
        let (uncertain, retained) = queue.into_recovery();
        assert_eq!(uncertain, Some(id));
        assert_eq!(retained.len(), 1);
    }
}
