//! Destination identity for ordered terminal input. No keyboard recovery queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Context {
    pub project: usize,
    pub attachment: u64,
}

#[derive(Clone, Copy)]
pub struct Route {
    pub context: Context,
    pub token: u64,
}

/// A quit request remains current only until the next user action.
#[derive(Default)]
pub struct QuitIntent {
    activity: u64,
    waiting: Option<u64>,
}
impl QuitIntent {
    pub fn request(&mut self) { self.waiting = Some(self.activity); }
    pub fn activity(&mut self) {
        self.activity = self.activity.wrapping_add(1);
        self.waiting = None;
    }
    pub fn cancel(&mut self) { self.activity(); }
    pub fn take_ready(&mut self, busy: bool) -> Option<u64> {
        if busy { None } else { self.waiting.take() }
    }
    pub fn is_current(&self, generation: u64) -> bool { self.activity == generation }
}
