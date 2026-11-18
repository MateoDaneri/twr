//! Identity and lifecycle of a file read. This module never owns user input.
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq)]
pub struct Context {
    pub workspace: usize,
    pub pane: u64,
    pub attachment: u64,
    pub generation: u64,
    pub path: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Reading,
    Loaded,
    Cancelled,
}
