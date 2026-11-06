//! Recoverable input held while a terminal key's destination is resolved.
//!
//! This ledger does not dispatch input or inspect terminal contents. The
//! owner must validate the original pane, process identity and focus epoch,
//! then bind replay to the destination produced by the acknowledged decision.

use crate::open_input::Event;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Inspecting,
    DecisionReady,
    DecisionInFlight,
    Replaying,
    Recover,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    VisualMotion,
    BufferCycle,
}

struct Entry<K> {
    id: u64,
    bytes: usize,
    event: Event<K>,
}

pub struct Recovery<K> {
    pub decision: Option<Decision>,
    pub decision_uncertain: bool,
    pub in_flight_event: Option<u64>,
    pub events: Vec<Event<K>>,
}

/// Tokens and event identities are never reused by one gate. Rejected input
/// belongs to the caller, which must retain it instead of dropping it.
pub struct Gate<K> {
    token: u64,
    state: State,
    decision: Option<Decision>,
    decision_dispatched: bool,
    events: VecDeque<Entry<K>>,
    next: Option<u64>,
    in_flight: Option<u64>,
    bytes: usize,
    max_events: usize,
    max_bytes: usize,
}

impl<K> Gate<K> {
    pub fn new(token: u64, max_events: usize, max_bytes: usize) -> Option<Self> {
        (token != 0 && max_events != 0 && max_bytes != 0).then(|| Self {
            token,
            state: State::Inspecting,
            decision: None,
            decision_dispatched: false,
            events: VecDeque::new(),
            next: Some(1),
            in_flight: None,
            bytes: 0,
            max_events,
            max_bytes,
        })
    }

    /// Queue an already targeted action and later input in dispatch order.
    pub fn for_replay(token: u64, max_events: usize, max_bytes: usize) -> Option<Self> {
        let mut gate = Self::new(token, max_events, max_bytes)?;
        gate.state = State::Replaying;
        Some(gate)
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub fn in_flight_id(&self) -> Option<u64> {
        self.in_flight
    }
    pub fn events(&self) -> impl Iterator<Item = &Event<K>> {
        self.events.iter().map(|entry| &entry.event)
    }

    /// The caller supplies the byte size for a key; text events count their
    /// own UTF-8 bytes. Rejecting an event never means it was dispatched.
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
        if self.events.len() >= self.max_events || total > self.max_bytes {
            return Err(event);
        }
        self.next = id.checked_add(1);
        self.bytes = total;
        self.events.push_back(Entry { id, bytes, event });
        Ok(())
    }

    /// `current` must include the original focus, attachment and tmux target.
    /// A stale token is ignored; an error or changed context retains input.
    pub fn inspected(&mut self, token: u64, current: bool, selection: Result<bool, ()>) -> bool {
        if token != self.token || self.state != State::Inspecting {
            return false;
        }
        if !current || selection.is_err() {
            self.state = State::Recover;
            return false;
        }
        self.decision = Some(if selection == Ok(true) {
            Decision::VisualMotion
        } else {
            Decision::BufferCycle
        });
        self.state = State::DecisionReady;
        true
    }

    pub fn begin_decision(&mut self, current: bool) -> Option<Decision> {
        if self.state != State::DecisionReady {
            return None;
        }
        if !current {
            self.state = State::Recover;
            return None;
        }
        self.state = State::DecisionInFlight;
        self.decision_dispatched = true;
        self.decision
    }

    /// An uncertain decision must never be executed twice automatically.
    pub fn finish_decision(&mut self, applied: Option<bool>) -> bool {
        if self.state != State::DecisionInFlight {
            return false;
        }
        if applied.is_some() {
            self.decision_dispatched = false;
        }
        self.state = match applied {
            Some(true) => State::Replaying,
            Some(false) => State::Recover,
            None => State::Uncertain,
        };
        applied == Some(true)
    }

    /// The caller checks the bound destination before every replay dispatch.
    pub fn begin_event(&mut self, current: bool) -> Option<(u64, &Event<K>)> {
        if self.state != State::Replaying || self.in_flight.is_some() {
            return None;
        }
        if !current {
            self.state = State::Recover;
            return None;
        }
        let entry = self.events.front()?;
        self.in_flight = Some(entry.id);
        Some((entry.id, &entry.event))
    }

    pub fn acknowledge(&mut self, id: u64) -> bool {
        if self.state != State::Replaying
            || self.in_flight != Some(id)
            || self.events.front().is_none_or(|entry| entry.id != id)
        {
            return false;
        }
        let entry = self.events.pop_front().expect("front checked");
        self.bytes -= entry.bytes;
        self.in_flight = None;
        true
    }

    /// A later H/L in the same ordered queue needs its own inspection. Only
    /// the event at the front may become the next decision; the remaining
    /// events stay in order and cannot be dispatched before it resolves.
    pub fn reinspect_after(&mut self, id: u64, token: u64) -> bool {
        if token == 0 || !self.acknowledge(id) {
            return false;
        }
        self.token = token;
        self.decision = None;
        self.decision_dispatched = false;
        self.state = State::Inspecting;
        true
    }

    /// The owner proved dispatch did not occur; retain the event for review.
    pub fn not_dispatched(&mut self, id: u64) -> bool {
        if self.state != State::Replaying || self.in_flight != Some(id) {
            return false;
        }
        self.in_flight = None;
        self.state = State::Recover;
        true
    }

    /// Dispatch may have happened. Retain it, but never repeat automatically.
    pub fn uncertain(&mut self, id: u64) -> bool {
        if self.in_flight != Some(id) {
            return false;
        }
        self.state = State::Uncertain;
        true
    }

    pub fn invalidate(&mut self) {
        self.state = if matches!(self.state, State::DecisionInFlight | State::Uncertain)
            || self.in_flight.is_some()
        {
            State::Uncertain
        } else {
            State::Recover
        };
    }

    /// The front event may already have executed if its id is returned here.
    pub fn into_recovery(self) -> Recovery<K> {
        Recovery {
            decision: self.decision,
            decision_uncertain: self.decision_dispatched,
            in_flight_event: self.in_flight,
            events: self.events.into_iter().map(|entry| entry.event).collect(),
        }
    }
}
