//! Guard a window-local jump across workspace buffers and asynchronous loads.
//! Planning never opens files or mutates the caller's layout or editor bank.
use crate::{
    buffer_numbers::BufferTracking,
    buffer_registry::{Buffer, BufferRegistry},
    editor::{Editor, EditorIdentity, LiveJumpTracking},
    editor_panes::{Content, Panes},
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use workspace_core::{editor_jumplist::Location, editor_panes::PaneJump};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JumpError {
    SourceUnavailable,
    DestinationUnavailable,
    NeedsSynchronization,
    LoadRequired,
    Stale,
}

/// A prepared request is bound to a pane, workspace epoch, source editor and
/// exact catalog identities. A missing destination must be loaded by the
/// caller through its normal guarded I/O before this request can be applied.
pub struct JumpRequest {
    workspace: usize,
    context_generation: u64,
    source: Buffer,
    source_identity: EditorIdentity,
    source_version: u64,
    origin: Location,
    target: Option<Buffer>,
    target_location: Option<Location>,
    target_editor: Option<(EditorIdentity, u64)>,
    awaiting_load: bool,
    plan: PaneJump,
}

#[derive(Debug, PartialEq, Eq)]
pub enum JumpApplied {
    /// Neovim may normalize history even when a requested count cannot move.
    NoMove,
    Move {
        target: Buffer,
        location: Location,
    },
}

impl JumpRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        panes: &Panes,
        numbers: &BufferTracking,
        tracking: &LiveJumpTracking,
        registry: &BufferRegistry,
        editors: &HashMap<PathBuf, Editor>,
        terminals: &HashSet<u64>,
        workspace: usize,
        context_generation: u64,
        origin: Location,
        older: bool,
        count: usize,
    ) -> Result<Option<Self>, JumpError> {
        let pane = panes.layout.active();
        let Some(Content::File(path)) = panes.content(pane) else {
            return Err(JumpError::SourceUnavailable);
        };
        let source = Buffer::File(path.clone());
        let source_editor = editors.get(path).ok_or(JumpError::SourceUnavailable)?;
        if !registry.knows(&source)
            || numbers.number_for(workspace, &source).ok() != Some(origin.buffer)
            || source_editor.position() != (origin.row, origin.column)
        {
            return Err(JumpError::SourceUnavailable);
        }
        if !tracking.is_current(origin.buffer, source_editor) {
            return Err(JumpError::NeedsSynchronization);
        }
        let Some(plan) = panes.layout.prepare_jump(origin, older, count) else {
            return Ok(None);
        };
        let target_location = plan.target();
        let target = target_location
            .map(|location| numbers.resolve(workspace, location.buffer))
            .transpose()
            .map_err(|_| JumpError::DestinationUnavailable)?;
        let (target_editor, awaiting_load) = match target.as_ref() {
            None => (None, false),
            Some(buffer) if !registry.knows(buffer) => {
                return Err(JumpError::DestinationUnavailable);
            }
            Some(Buffer::File(path)) => match editors.get(path) {
                Some(editor) => {
                    if !tracking
                        .is_current(target_location.expect("target has location").buffer, editor)
                    {
                        return Err(JumpError::NeedsSynchronization);
                    }
                    (Some((editor.identity(), editor.version)), false)
                }
                None if registry.is_unloaded(path) => (None, true),
                None => return Err(JumpError::DestinationUnavailable),
            },
            Some(Buffer::Terminal(id)) if terminals.contains(id) => (None, false),
            Some(Buffer::Terminal(_)) => return Err(JumpError::DestinationUnavailable),
            Some(Buffer::Explorer(_) | Buffer::Agents(_)) => {
                return Err(JumpError::DestinationUnavailable);
            }
        };
        Ok(Some(Self {
            workspace,
            context_generation,
            source,
            source_identity: source_editor.identity(),
            source_version: source_editor.version,
            origin,
            target,
            target_location,
            target_editor,
            awaiting_load,
            plan,
        }))
    }

    pub fn target(&self) -> Option<&Buffer> {
        self.target.as_ref()
    }
    pub fn target_anchor(&self) -> Option<workspace_core::editor_jumplist::AnchorId> {
        self.plan.target_anchor()
    }
    pub fn target_location(&self) -> Option<Location> {
        self.target_location
    }
    pub fn needs_load(&self) -> bool {
        self.awaiting_load
    }

    /// A planned movement invalidates older asynchronous file operations.
    /// A no-move history normalization keeps the current input generation.
    pub fn advance_context_generation(&mut self, next: u64) -> bool {
        if self.target.is_none() || next != self.context_generation.wrapping_add(1) {
            return false;
        }
        self.context_generation = next;
        true
    }

    /// Before starting or completing a load, verify the original editor and
    /// history without changing either. This also detects focus away and back.
    #[allow(clippy::too_many_arguments)]
    pub fn source_is_current(
        &self,
        panes: &Panes,
        numbers: &BufferTracking,
        tracking: &LiveJumpTracking,
        registry: &BufferRegistry,
        editors: &HashMap<PathBuf, Editor>,
        workspace: usize,
        context_generation: u64,
        current: Location,
    ) -> Result<(), JumpError> {
        let pane = panes.layout.active();
        let source_editor = match &self.source {
            Buffer::File(path) => editors.get(path),
            Buffer::Terminal(_) | Buffer::Explorer(_) | Buffer::Agents(_) => None,
        }
        .ok_or(JumpError::Stale)?;
        if workspace != self.workspace
            || context_generation != self.context_generation
            || current != self.origin
            || !self.source_identity.matches(source_editor)
            || source_editor.version != self.source_version
            || source_editor.position() != (current.row, current.column)
            || numbers.resolve(workspace, current.buffer).ok().as_ref() != Some(&self.source)
            || !registry.knows(&self.source)
            || !matches!((panes.content(pane), &self.source), (Some(Content::File(a)), Buffer::File(b)) if a == b)
        {
            return Err(JumpError::Stale);
        }
        if !tracking.is_current(current.buffer, source_editor) {
            return Err(JumpError::NeedsSynchronization);
        }
        let mut staged = panes.clone();
        if !staged.layout.commit_jump(self.plan.clone(), current) {
            return Err(JumpError::Stale);
        }
        Ok(())
    }

    /// Check the source and the unloaded target again before consuming a
    /// completed read. This does not create an editor or alter the jump list.
    #[allow(clippy::too_many_arguments)]
    pub fn load_is_current(
        &self,
        panes: &Panes,
        numbers: &BufferTracking,
        tracking: &LiveJumpTracking,
        registry: &BufferRegistry,
        editors: &HashMap<PathBuf, Editor>,
        workspace: usize,
        context_generation: u64,
        current: Location,
        path: &Path,
    ) -> Result<u64, JumpError> {
        self.source_is_current(
            panes,
            numbers,
            tracking,
            registry,
            editors,
            workspace,
            context_generation,
            current,
        )?;
        let location = self.target_location.ok_or(JumpError::Stale)?;
        let target = Buffer::File(path.to_owned());
        if !self.awaiting_load
            || self.target.as_ref() != Some(&target)
            || numbers.resolve(workspace, location.buffer).ok().as_ref() != Some(&target)
            || !registry.is_unloaded(path)
            || editors.contains_key(path)
            || !tracking.is_suspended(location.buffer)
        {
            return Err(JumpError::Stale);
        }
        Ok(location.buffer)
    }

    /// Called only after the normal guarded file loader verified this path.
    /// Applying the request still rechecks focus, workspace, source and target.
    pub fn accept_loaded_file(
        &mut self,
        workspace: usize,
        context_generation: u64,
        path: &Path,
        editor: &Editor,
    ) -> Result<(), JumpError> {
        if !self.awaiting_load
            || self.workspace != workspace
            || self.context_generation != context_generation
            || !matches!(&self.target, Some(Buffer::File(target)) if target == path)
        {
            return Err(JumpError::Stale);
        }
        self.target_editor = Some((editor.identity(), editor.version));
        self.awaiting_load = false;
        Ok(())
    }

    /// Revalidate and commit in one borrow. The returned location is applied
    /// by the UI only after it has preflighted terminal focus and geometry.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        panes: &mut Panes,
        numbers: &BufferTracking,
        tracking: &LiveJumpTracking,
        registry: &mut BufferRegistry,
        editors: &HashMap<PathBuf, Editor>,
        terminals: &HashSet<u64>,
        workspace: usize,
        context_generation: u64,
        current: Location,
    ) -> Result<JumpApplied, JumpError> {
        if self.awaiting_load {
            return Err(JumpError::LoadRequired);
        }
        let pane = panes.layout.active();
        self.source_is_current(
            panes,
            numbers,
            tracking,
            registry,
            editors,
            workspace,
            context_generation,
            current,
        )?;
        if let Some(target) = &self.target {
            let location = self.target_location.ok_or(JumpError::Stale)?;
            if numbers.resolve(workspace, location.buffer).ok().as_ref() != Some(target)
                || !registry.knows(target)
            {
                return Err(JumpError::DestinationUnavailable);
            }
            match target {
                Buffer::File(path) => {
                    let editor = editors.get(path).ok_or(JumpError::DestinationUnavailable)?;
                    let (identity, version) =
                        self.target_editor.as_ref().ok_or(JumpError::LoadRequired)?;
                    if !identity.matches(editor) || *version != editor.version {
                        return Err(JumpError::Stale);
                    }
                    if !tracking.is_current(location.buffer, editor) {
                        return Err(JumpError::NeedsSynchronization);
                    }
                }
                Buffer::Terminal(id) if !terminals.contains(id) => {
                    return Err(JumpError::DestinationUnavailable);
                }
                Buffer::Terminal(_) => {}
                Buffer::Explorer(_) | Buffer::Agents(_) => {
                    return Err(JumpError::DestinationUnavailable);
                }
            }
        }
        let mut staged_panes = panes.clone();
        let mut staged_registry = registry.clone();
        if !staged_panes.layout.commit_jump(self.plan.clone(), current) {
            return Err(JumpError::Stale);
        }
        let result = match (&self.target, self.target_location) {
            (None, None) => JumpApplied::NoMove,
            (Some(target), Some(location)) => {
                if target != &self.source {
                    staged_panes
                        .set_content(match target {
                            Buffer::File(path) => Content::File(path.clone()),
                            Buffer::Terminal(id) => Content::Terminal(*id),
                            Buffer::Explorer(_) | Buffer::Agents(_) => {
                                return Err(JumpError::DestinationUnavailable);
                            }
                        })
                        .map_err(|_| JumpError::Stale)?;
                    staged_registry.replace(pane, Some(&self.source), target.clone());
                    // Unlike :buffer N, a cross-buffer jump lists its target.
                    staged_registry.relist(target);
                }
                JumpApplied::Move {
                    target: target.clone(),
                    location,
                }
            }
            _ => return Err(JumpError::Stale),
        };
        *panes = staged_panes;
        *registry = staged_registry;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor_panes::{Axis, ViewState, stage_listed_buffer_switch};
    use workspace_core::editor_jumplist::View;

    struct Fixture {
        a: Buffer,
        b: Buffer,
        a_path: PathBuf,
        b_path: PathBuf,
        numbers: BufferTracking,
        tracking: LiveJumpTracking,
        registry: BufferRegistry,
        panes: Panes,
        editors: HashMap<PathBuf, Editor>,
        terminals: HashSet<u64>,
        current: Location,
    }
    fn fixture() -> Fixture {
        let a_path = PathBuf::from("/synthetic/a.txt");
        let b_path = PathBuf::from("/synthetic/b.txt");
        let a = Buffer::File(a_path.clone());
        let b = Buffer::File(b_path.clone());
        let mut numbers = BufferTracking::default();
        numbers.register(1, a.clone());
        numbers.register(1, b.clone());
        let mut registry = BufferRegistry::default();
        registry.register(a.clone());
        registry.register(b.clone());
        let mut initial = Panes::new(
            a_path.clone(),
            ViewState {
                buffer: 0,
                row: 1,
                column: 1,
                top: 0,
                left: 0,
            },
        );
        let mut source = Editor::new("a1\na2\na3".into());
        source.goto(1, 1);
        let mut editors = HashMap::from([
            (a_path.clone(), source),
            (b_path.clone(), Editor::new("b1\nb2\nb3".into())),
        ]);
        let mut tracking = LiveJumpTracking::default();
        for buffer in [&a, &b] {
            let Buffer::File(path) = buffer else {
                unreachable!()
            };
            let number = numbers.number_for(1, buffer).unwrap();
            assert_eq!(
                tracking.sync(
                    number,
                    editors.get_mut(path).unwrap(),
                    None,
                    &mut [&mut initial.layout]
                ),
                crate::editor::JumpApplyOutcome::Applied
            );
        }
        let panes = stage_listed_buffer_switch(&initial, &numbers, 1, b.clone()).unwrap();
        let current = Location {
            buffer: numbers.number_for(1, &b).unwrap(),
            row: 0,
            column: 0,
            view: View {
                top: 0,
                left: 0,
                skip: 0,
                valid: true,
            },
        };
        Fixture {
            a,
            b,
            a_path,
            b_path,
            numbers,
            tracking,
            registry,
            panes,
            editors,
            terminals: HashSet::new(),
            current,
        }
    }
    fn prepare(f: &Fixture) -> JumpRequest {
        JumpRequest::prepare(
            &f.panes,
            &f.numbers,
            &f.tracking,
            &f.registry,
            &f.editors,
            &f.terminals,
            1,
            7,
            f.current,
            true,
            1,
        )
        .unwrap()
        .unwrap()
    }
    #[test]
    fn cross_buffer_jump_and_return_commit_once_without_adding_extra_marks() {
        let mut f = fixture();
        let request = prepare(&f);
        assert_eq!(request.target(), Some(&f.a));
        let JumpApplied::Move { target, location } = request
            .apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current,
            )
            .unwrap()
        else {
            panic!("expected move")
        };
        assert_eq!(target, f.a);
        assert_eq!((location.row, location.column), (1, 1));
        assert_eq!(f.panes.path(f.panes.layout.active()), Some(&f.a_path));
        assert_eq!(
            f.registry.alternate(f.panes.layout.active(), &f.a),
            Some(f.b.clone())
        );
        assert_eq!(
            request.apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current
            ),
            Err(JumpError::Stale)
        );
        f.editors
            .get_mut(&f.a_path)
            .unwrap()
            .goto(location.row, location.column);
        let forward = JumpRequest::prepare(
            &f.panes,
            &f.numbers,
            &f.tracking,
            &f.registry,
            &f.editors,
            &f.terminals,
            1,
            7,
            location,
            false,
            1,
        )
        .unwrap()
        .unwrap();
        let JumpApplied::Move { target, location } = forward
            .apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                location,
            )
            .unwrap()
        else {
            panic!("expected return")
        };
        assert_eq!(target, f.b);
        assert_eq!((location.row, location.column), (0, 0));
        assert_eq!(f.panes.path(f.panes.layout.active()), Some(&f.b_path));
    }
    #[test]
    fn changed_focus_context_source_or_target_rejects_without_mutation() {
        for change in [
            "focus",
            "focus-return",
            "generation",
            "source",
            "target",
            "queued-target-jump",
            "identity",
            "number",
        ] {
            let mut f = fixture();
            let request = prepare(&f);
            let mut generation = 7;
            match change {
                "focus" => {
                    f.panes.layout.split(Axis::Row, true).unwrap();
                }
                "focus-return" => {
                    let original = f.panes.layout.active();
                    f.panes.layout.split(Axis::Row, true).unwrap();
                    f.panes.layout.focus(original).unwrap();
                }
                "generation" => generation += 1,
                "source" => {
                    f.editors.get_mut(&f.b_path).unwrap().key("i");
                    f.editors.get_mut(&f.b_path).unwrap().insert("x");
                }
                "target" => {
                    f.editors.get_mut(&f.a_path).unwrap().key("i");
                    f.editors.get_mut(&f.a_path).unwrap().insert("x");
                }
                "queued-target-jump" => {
                    f.editors.get_mut(&f.a_path).unwrap().key("G");
                }
                "identity" => {
                    f.editors
                        .insert(f.a_path.clone(), Editor::new("new".into()));
                }
                "number" => f
                    .numbers
                    .rebind(&f.a, Buffer::File("/synthetic/renamed".into()), None),
                _ => unreachable!(),
            }
            let before = f.panes.content(f.panes.layout.active()).cloned();
            let result = request.apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                generation,
                f.current,
            );
            assert!(result.is_err(), "{change}");
            assert_eq!(
                f.panes.content(f.panes.layout.active()).cloned(),
                before,
                "{change}"
            );
        }
    }
    #[test]
    fn source_guard_rejects_focus_round_trip_and_edits_before_loading() {
        for change in ["none", "focus-return", "generation", "edit", "cursor"] {
            let mut f = fixture();
            let request = prepare(&f);
            let mut generation = 7;
            match change {
                "focus-return" => {
                    let original = f.panes.layout.active();
                    f.panes.layout.split(Axis::Row, true).unwrap();
                    f.panes.layout.focus(original).unwrap();
                }
                "generation" => generation += 1,
                "edit" => {
                    let editor = f.editors.get_mut(&f.b_path).unwrap();
                    editor.key("i");
                    editor.insert("x");
                }
                "cursor" => f.editors.get_mut(&f.b_path).unwrap().goto(1, 0),
                "none" => {}
                _ => unreachable!(),
            }
            let before = f.panes.content(f.panes.layout.active()).cloned();
            let result = request.source_is_current(
                &f.panes,
                &f.numbers,
                &f.tracking,
                &f.registry,
                &f.editors,
                1,
                generation,
                f.current,
            );
            assert_eq!(result.is_ok(), change == "none", "{change}");
            assert_eq!(f.panes.content(f.panes.layout.active()).cloned(), before);
        }
    }
    #[test]
    fn pending_target_edits_must_be_synchronized_before_planning() {
        let mut f = fixture();
        f.editors.get_mut(&f.a_path).unwrap().key("i");
        f.editors.get_mut(&f.a_path).unwrap().insert("x\n");
        assert!(matches!(
            JumpRequest::prepare(
                &f.panes,
                &f.numbers,
                &f.tracking,
                &f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current,
                true,
                1
            ),
            Err(JumpError::NeedsSynchronization)
        ));
    }
    #[test]
    fn unloaded_unlisted_target_needs_matching_load_and_becomes_listed() {
        let mut f = fixture();
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-unload.json"
        ))
        .unwrap();
        let case = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == "bdelete")
            .unwrap();
        assert_eq!(case["before"]["jumps"][0]["listed"], 0);
        assert_eq!(case["after"]["jumps"][0]["listed"], 1);
        // With jumpoptions=view, :bdelete retains the unloaded buffer's mark.
        let id = f.numbers.number_for(1, &f.a).unwrap();
        let mut old = f.editors.remove(&f.a_path).unwrap();
        f.tracking
            .suspend(id, Some(&mut old), &mut [&mut f.panes.layout]);
        assert!(f.registry.unlist_file(&f.a_path));
        let mut request = prepare(&f);
        assert!(request.needs_load());
        assert_eq!(
            request.load_is_current(
                &f.panes,
                &f.numbers,
                &f.tracking,
                &f.registry,
                &f.editors,
                1,
                7,
                f.current,
                &f.a_path
            ),
            Ok(id)
        );
        assert_eq!(
            request.load_is_current(
                &f.panes,
                &f.numbers,
                &f.tracking,
                &f.registry,
                &f.editors,
                1,
                7,
                f.current,
                Path::new("/synthetic/wrong")
            ),
            Err(JumpError::Stale)
        );
        assert_eq!(
            request.load_is_current(
                &f.panes,
                &f.numbers,
                &f.tracking,
                &f.registry,
                &f.editors,
                1,
                8,
                f.current,
                &f.a_path
            ),
            Err(JumpError::Stale)
        );
        assert_eq!(
            request.source_is_current(
                &f.panes,
                &f.numbers,
                &f.tracking,
                &f.registry,
                &f.editors,
                1,
                7,
                f.current
            ),
            Ok(())
        );
        assert_eq!(
            request.source_is_current(
                &f.panes,
                &f.numbers,
                &f.tracking,
                &f.registry,
                &f.editors,
                1,
                8,
                f.current
            ),
            Err(JumpError::Stale)
        );
        assert_eq!(
            request.apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current
            ),
            Err(JumpError::LoadRequired)
        );
        let mut editor = Editor::new("loaded".into());
        assert_eq!(
            request.accept_loaded_file(1, 8, &f.a_path, &editor),
            Err(JumpError::Stale)
        );
        assert_eq!(
            request.accept_loaded_file(1, 7, Path::new("/synthetic/wrong"), &editor),
            Err(JumpError::Stale)
        );
        request
            .accept_loaded_file(1, 7, &f.a_path, &editor)
            .unwrap();
        assert!(f.tracking.resume(id, &mut editor));
        f.editors.insert(f.a_path.clone(), editor);
        f.registry.mark_loaded(&f.a_path);
        assert!(matches!(
            request.apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current
            ),
            Ok(JumpApplied::Move { .. })
        ));
        assert!(f.registry.listed().contains(&f.a));
    }

    #[test]
    fn jumping_to_loaded_unlisted_buffer_lists_it() {
        let mut f = fixture();
        assert!(f.registry.unlist_file(&f.a_path));
        f.registry.mark_loaded(&f.a_path);
        let request = prepare(&f);
        assert!(matches!(
            request.apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current
            ),
            Ok(JumpApplied::Move { .. })
        ));
        assert!(f.registry.listed().contains(&f.a));
    }
    #[test]
    fn delayed_jump_load_rejects_changed_focus_and_target_without_mutating_history() {
        for change in [
            "focus-return",
            "source-edit",
            "target-loaded",
            "target-rebound",
        ] {
            let mut f = fixture();
            let id = f.numbers.number_for(1, &f.a).unwrap();
            let mut old = f.editors.remove(&f.a_path).unwrap();
            f.tracking
                .suspend(id, Some(&mut old), &mut [&mut f.panes.layout]);
            assert!(f.registry.unlist_file(&f.a_path));
            let request = prepare(&f);
            match change {
                "focus-return" => {
                    let original = f.panes.layout.active();
                    f.panes.layout.split(Axis::Row, true).unwrap();
                    f.panes.layout.focus(original).unwrap();
                }
                "source-edit" => {
                    let source = f.editors.get_mut(&f.b_path).unwrap();
                    source.key("i");
                    source.insert("x");
                }
                "target-loaded" => {
                    f.registry.mark_loaded(&f.a_path);
                }
                "target-rebound" => {
                    f.numbers
                        .rebind(&f.a, Buffer::File("/synthetic/new".into()), None)
                }
                _ => unreachable!(),
            }
            let pane = f.panes.layout.active();
            let before = f.panes.content(pane).cloned();
            assert!(
                request
                    .load_is_current(
                        &f.panes,
                        &f.numbers,
                        &f.tracking,
                        &f.registry,
                        &f.editors,
                        1,
                        7,
                        f.current,
                        &f.a_path
                    )
                    .is_err(),
                "{change}"
            );
            assert_eq!(f.panes.content(pane).cloned(), before, "{change}");
            assert!(f.tracking.is_suspended(id), "{change}");
        }
    }
    #[test]
    fn jump_within_same_unlisted_buffer_does_not_relist_it() {
        let mut f = fixture();
        assert!(f.registry.unlist_file(&f.b_path));
        f.registry.mark_loaded(&f.b_path);
        let active = f.panes.layout.active();
        let history = f.panes.layout.jump_list_mut(active).unwrap();
        history.clear();
        history.record(Location {
            buffer: f.current.buffer,
            row: 1,
            column: 0,
            view: View::default(),
        });
        let request = prepare(&f);
        assert!(matches!(
            request.apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current
            ),
            Ok(JumpApplied::Move { .. })
        ));
        assert!(f.registry.is_unlisted(&f.b));
    }
    #[test]
    fn reloaded_file_cursor_clamps_like_neovim_without_rewriting_mark() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-jumplist-reload.json"
        ))
        .unwrap();
        for case in reference["cases"].as_array().unwrap() {
            let mut f = fixture();
            let id = f.numbers.number_for(1, &f.a).unwrap();
            let mut old = f.editors.remove(&f.a_path).unwrap();
            f.tracking
                .suspend(id, Some(&mut old), &mut [&mut f.panes.layout]);
            assert!(f.registry.unlist_file(&f.a_path));
            let mut request = prepare(&f);
            let name = case["name"].as_str().unwrap();
            let text = match name {
                "unchanged" => "a1\na2\na3",
                "prepend" => "x1\nx2\na1\na2\na3",
                "truncate" => "x",
                _ => panic!("unknown case"),
            };
            let mut loaded = Editor::new(text.into());
            request
                .accept_loaded_file(1, 7, &f.a_path, &loaded)
                .unwrap();
            assert!(f.tracking.resume(id, &mut loaded));
            f.editors.insert(f.a_path.clone(), loaded);
            f.registry.mark_loaded(&f.a_path);
            let JumpApplied::Move { location, .. } = request
                .apply(
                    &mut f.panes,
                    &f.numbers,
                    &f.tracking,
                    &mut f.registry,
                    &f.editors,
                    &f.terminals,
                    1,
                    7,
                    f.current,
                )
                .unwrap()
            else {
                panic!("expected file jump")
            };
            assert_eq!(
                (location.row + 1, location.column),
                (
                    case["after"]["jumps"][0]["row"].as_u64().unwrap() as usize,
                    case["after"]["jumps"][0]["col"].as_u64().unwrap() as usize
                ),
                "mark {name}"
            );
            let editor = f.editors.get_mut(&f.a_path).unwrap();
            editor.goto(location.row, location.column);
            assert_eq!(
                editor.position(),
                (
                    case["after"]["cursor"][0].as_u64().unwrap() as usize - 1,
                    case["after"]["cursor"][1].as_u64().unwrap() as usize
                ),
                "cursor {name}"
            );
            assert!(f.registry.listed().contains(&f.a));
        }
    }

    #[test]
    fn terminal_destination_must_remain_available_in_the_same_workspace() {
        let mut f = fixture();
        let terminal = Buffer::Terminal(77);
        f.numbers.register(1, terminal.clone());
        f.registry.register(terminal.clone());
        f.terminals.insert(77);
        let active = f.panes.layout.active();
        let history = f.panes.layout.jump_list_mut(active).unwrap();
        history.clear();
        let anchor = workspace_core::editor_jumplist::AnchorId::new(123).unwrap();
        history.record_with_anchor(
            Location {
                buffer: f.numbers.number_for(1, &terminal).unwrap(),
                row: 6,
                column: 3,
                view: View::default(),
            },
            Some(anchor),
        );
        let request = prepare(&f);
        assert_eq!(request.target(), Some(&terminal));
        assert_eq!(request.target_anchor(), Some(anchor));
        f.terminals.remove(&77);
        assert_eq!(
            request.apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current
            ),
            Err(JumpError::DestinationUnavailable)
        );
        assert_eq!(
            f.panes.content(f.panes.layout.active()),
            Some(&Content::File(f.b_path.clone()))
        );
        f.terminals.insert(77);
        assert!(matches!(
            request.apply(
                &mut f.panes,
                &f.numbers,
                &f.tracking,
                &mut f.registry,
                &f.editors,
                &f.terminals,
                1,
                7,
                f.current
            ),
            Ok(JumpApplied::Move {
                target: Buffer::Terminal(77),
                ..
            })
        ));
        assert_eq!(
            f.panes.content(f.panes.layout.active()),
            Some(&Content::Terminal(77))
        );
    }
}
