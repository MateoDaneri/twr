//! Listed buffer identities are separate from windows and terminal processes.
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
};
use workspace_core::editor_panes::PaneId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Buffer {
    File(PathBuf),
    Terminal(u64),
    /// An Explorer view has a buffer identity without joining listed order.
    Explorer(u64),
    /// Agents view state is distinct from files and terminal attachments.
    Agents(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub enum VimDeletePermission {
    Ready,
    Modified,
    Busy,
}

pub fn vim_delete_permission(dirty: bool, force: bool, busy: bool) -> VimDeletePermission {
    if busy {
        VimDeletePermission::Busy
    } else if dirty && !force {
        VimDeletePermission::Modified
    } else {
        VimDeletePermission::Ready
    }
}

/// LazyVim's H/L buffer mappings perform one cycle even with a count.
pub fn normal_buffer_cycle(chord: &str, pending: bool, count: &mut usize) -> Option<bool> {
    if pending {
        return None;
    }
    let forward = match chord {
        "H" => false,
        "L" => true,
        _ => return None,
    };
    *count = 0;
    Some(forward)
}

/// Forget a deleted file without making its replacement belong to other workspaces.
pub fn forget_deleted_file(
    path: &Path,
    owners: &mut HashMap<PathBuf, BTreeSet<usize>>,
    last_editor: &mut HashMap<usize, PathBuf>,
) {
    owners.remove(path);
    last_editor.retain(|_, editor| editor != path);
}

/// Detach one workspace without deleting a buffer used by another workspace.
/// A visible saved view is enough to preserve the editor even if ownership
/// metadata is incomplete.
pub fn detach_workspace_file(
    path: &Path,
    project: usize,
    owners: &mut HashMap<PathBuf, BTreeSet<usize>>,
    last_editor: &mut HashMap<usize, PathBuf>,
    other_views: &BTreeSet<usize>,
) -> bool {
    let shared = !other_views.is_empty()
        || owners
            .get(path)
            .is_some_and(|projects| projects.iter().any(|owner| *owner != project));
    if shared {
        let projects = owners.entry(path.to_owned()).or_default();
        projects.remove(&project);
        projects.extend(other_views.iter().copied());
        if last_editor.get(&project).is_some_and(|last| last == path) {
            last_editor.remove(&project);
        }
    }
    shared
}

/// A confirmation may only apply to the exact version and workspace shown.
#[derive(Clone)]
pub struct DeleteGuard {
    pub path: PathBuf,
    pub project: usize,
    pub version: u64,
}
impl DeleteGuard {
    pub fn permits(
        &self,
        path: Option<&Path>,
        project: usize,
        version: Option<u64>,
        busy: bool,
    ) -> bool {
        !busy
            && path == Some(self.path.as_path())
            && project == self.project
            && version == Some(self.version)
    }
}

/// Permission to delete is revoked permanently by any failed or stale save.
#[derive(Clone)]
pub struct DeleteAfterSave {
    pub guard: DeleteGuard,
    confirmed: bool,
    invalid: bool,
}
impl DeleteAfterSave {
    pub fn new(guard: DeleteGuard) -> Self {
        Self {
            guard,
            confirmed: false,
            invalid: false,
        }
    }
    pub fn saved(&mut self, success: bool, version: u64, current: Option<u64>) {
        if !success || version != self.guard.version || current != Some(version) {
            self.invalid = true;
        }
        self.confirmed = !self.invalid;
    }
    pub fn formatting(&mut self, valid: bool) {
        if !valid {
            self.invalid = true;
            self.confirmed = false;
        }
    }
    pub fn formatted_version(&mut self, version: u64) {
        if self.confirmed && !self.invalid {
            self.guard.version = version;
        }
    }
    pub fn permits(
        &self,
        path: Option<&Path>,
        project: usize,
        version: Option<u64>,
        dirty: bool,
    ) -> bool {
        self.confirmed
            && !self.invalid
            && !dirty
            && self.guard.permits(path, project, version, false)
    }
}

#[derive(Clone, Default)]
pub struct BufferRegistry {
    listed: Vec<Buffer>,
    known: Vec<Buffer>,
    line_order: Vec<Buffer>,
    alternate: BTreeMap<PaneId, Buffer>,
    recent: Vec<Buffer>,
    unloaded: BTreeSet<PathBuf>,
}

impl BufferRegistry {
    pub fn listed(&self) -> &[Buffer] {
        &self.listed
    }

    pub fn known(&self) -> &[Buffer] {
        &self.known
    }

    /// BufferLine's visual order is independent of Neovim's buffer numbers.
    pub fn line_order(&self) -> &[Buffer] {
        &self.line_order
    }

    /// Last activated listed terminal whose live attachment is still available.
    pub fn recent_terminal(&self, available: impl Fn(u64) -> bool) -> Option<u64> {
        self.recent.iter().rev().find_map(|buffer| match buffer {
            Buffer::Terminal(id) if available(*id) => Some(*id),
            _ => None,
        })
    }

    pub fn knows(&self, buffer: &Buffer) -> bool {
        self.known.contains(buffer)
    }

    /// Only a file listed in this workspace can be a fallback for its pane.
    pub fn fallback_file(&self, removed: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
        self.listed.iter().find_map(|buffer| match buffer {
            Buffer::File(path) if path != removed && exists(path) => Some(path.clone()),
            _ => None,
        })
    }
    pub fn register(&mut self, buffer: Buffer) {
        if matches!(buffer, Buffer::Explorer(_) | Buffer::Agents(_)) {
            self.register_unlisted(buffer);
            return;
        }
        if !self.known.contains(&buffer) {
            self.known.push(buffer.clone());
            self.listed.push(buffer.clone());
            self.line_order.push(buffer.clone());
            self.recent.push(buffer);
        }
    }

    /// Keep a sidebar addressable as an alternate without listing it.
    pub fn register_unlisted(&mut self, buffer: Buffer) {
        if !self.known.contains(&buffer) {
            self.known.push(buffer.clone());
            self.line_order.push(buffer);
        }
    }

    /// Explicitly editing an unlisted buffer lists it again, retaining its
    /// original position. Merely displaying it with :buffer does not.
    pub fn relist(&mut self, buffer: &Buffer) -> bool {
        if matches!(buffer, Buffer::Explorer(_) | Buffer::Agents(_)) {
            return false;
        }
        if !self.known.contains(buffer) || self.listed.contains(buffer) {
            return false;
        }
        self.listed.push(buffer.clone());
        self.listed
            .sort_by_key(|entry| self.known.iter().position(|known| known == entry));
        self.touch(buffer);
        true
    }

    pub fn is_unlisted(&self, buffer: &Buffer) -> bool {
        self.known.contains(buffer) && !self.listed.contains(buffer)
    }

    /// bdelete of a named file retains its identity and window alternates.
    /// The caller must first close every view and resolve pending writes.
    pub fn unlist_file(&mut self, path: &Path) -> bool {
        let buffer = Buffer::File(path.to_owned());
        if !self.known.contains(&buffer) {
            return false;
        }
        self.listed.retain(|entry| entry != &buffer);
        self.recent.retain(|entry| entry != &buffer);
        self.unloaded.insert(path.to_owned());
        true
    }

    /// Hide an already loaded file from ordinary buffer cycling while keeping
    /// its identity and any window-local alternate intact.
    pub fn unlist_loaded_file(&mut self, path: &Path) -> bool {
        let buffer = Buffer::File(path.to_owned());
        if !self.listed.contains(&buffer) || self.unloaded.contains(path) {
            return false;
        }
        self.listed.retain(|entry| entry != &buffer);
        self.recent.retain(|entry| entry != &buffer);
        true
    }

    /// A tree move also affects hidden, unloaded identities that have no
    /// Editor to carry the rename. Never merge colliding identities.
    pub fn move_unrepresented_files(
        &mut self,
        from: &Path,
        to: &Path,
        represented: impl Fn(&Path) -> bool,
    ) -> bool {
        let changes: Vec<_> = self
            .known
            .iter()
            .filter_map(|buffer| {
                let Buffer::File(path) = buffer else {
                    return None;
                };
                let tail = path.strip_prefix(from).ok()?;
                if represented(path) {
                    return None;
                }
                let target = if tail.as_os_str().is_empty() {
                    to.to_owned()
                } else {
                    to.join(tail)
                };
                Some((buffer.clone(), Buffer::File(target)))
            })
            .collect();
        if changes
            .iter()
            .any(|(source, target)| source != target && self.knows(target))
        {
            // Keep disk data, but do not leave an alternate pointing at the
            // old path after the filesystem move has already completed.
            for (source, _) in &changes {
                self.remove(source);
            }
            return false;
        }
        for (source, target) in changes {
            self.rebind(&source, target);
        }
        true
    }

    /// Record actual activation, independently of listing order.
    pub fn touch(&mut self, buffer: &Buffer) {
        if self.listed.contains(buffer) {
            self.recent.retain(|entry| entry != buffer);
            self.recent.push(buffer.clone());
        }
    }

    /// Record a replacement in one window; other windows keep their alternate.
    pub fn replace(&mut self, pane: PaneId, old: Option<&Buffer>, new: Buffer) {
        self.register(new.clone());
        if let Some(old) = old.filter(|old| **old != new && self.known.contains(old)) {
            self.alternate.insert(pane, old.clone());
        }
        self.touch(&new);
    }

    /// After Snacks replaces every view of a buffer, each affected window's
    /// alternate becomes the buffer that was just removed from that view.
    pub fn record_replacements(
        &mut self,
        old: &Buffer,
        replacements: impl IntoIterator<Item = (PaneId, Buffer)>,
    ) {
        self.register(old.clone());
        for (pane, new) in replacements {
            self.replace(pane, Some(old), new);
        }
    }

    pub fn cycle(&self, current: &Buffer, forward: bool) -> Option<Buffer> {
        self.cycle_available(current, forward, |_| true)
    }

    /// Walk listed order once, skipping files or attachments that disappeared.
    pub fn cycle_available(
        &self,
        current: &Buffer,
        forward: bool,
        available: impl Fn(&Buffer) -> bool,
    ) -> Option<Buffer> {
        let index = self.known.iter().position(|buffer| buffer == current)?;
        let len = self.known.len();
        (1..len).find_map(|step| {
            let next = if forward {
                (index + step) % len
            } else {
                (index + len - step) % len
            };
            self.known
                .get(next)
                .filter(|buffer| self.listed.contains(buffer) && available(buffer))
                .cloned()
        })
    }

    /// BufferLine groups pinned entries before the other buffers; :bnext and
    /// :bprevious continue to use the registry's ordinary order.
    pub fn cycle_pinned_first(
        &self,
        current: &Buffer,
        forward: bool,
        available: impl Fn(&Buffer) -> bool,
        pinned: impl Fn(&Buffer) -> bool,
    ) -> Option<Buffer> {
        let ordered: Vec<_> = self
            .line_order
            .iter()
            .filter(|buffer| pinned(buffer))
            .chain(self.line_order.iter().filter(|buffer| !pinned(buffer)))
            .collect();
        let index = ordered.iter().position(|buffer| *buffer == current)?;
        let len = ordered.len();
        (1..len).find_map(|step| {
            let next = if forward {
                (index + step) % len
            } else {
                (index + len - step) % len
            };
            let buffer = ordered[next];
            (self.listed.contains(buffer) && available(buffer)).then(|| buffer.clone())
        })
    }

    /// Swap adjacent visible BufferLine entries without changing :bnext order.
    pub fn move_line_buffer(
        &mut self,
        current: &Buffer,
        forward: bool,
        available: impl Fn(&Buffer) -> bool,
        pinned: impl Fn(&Buffer) -> bool,
    ) -> bool {
        let ordered: Vec<_> = self
            .line_order
            .iter()
            .enumerate()
            .filter(|(_, buffer)| {
                self.listed.contains(buffer) && available(buffer) && pinned(buffer)
            })
            .chain(self.line_order.iter().enumerate().filter(|(_, buffer)| {
                self.listed.contains(buffer) && available(buffer) && !pinned(buffer)
            }))
            .map(|(index, buffer)| (index, buffer.clone()))
            .collect();
        let Some(index) = ordered.iter().position(|(_, buffer)| buffer == current) else {
            return false;
        };
        let next = if forward {
            index.checked_add(1).filter(|next| *next < ordered.len())
        } else {
            index.checked_sub(1)
        };
        let Some(next) = next else {
            return false;
        };
        self.line_order.swap(ordered[index].0, ordered[next].0);
        true
    }

    pub fn alternate(&self, pane: PaneId, current: &Buffer) -> Option<Buffer> {
        self.alternate
            .get(&pane)
            .filter(|target| *target != current && self.known.contains(target))
            .cloned()
    }

    pub fn remove(&mut self, buffer: &Buffer) {
        self.listed.retain(|entry| entry != buffer);
        self.known.retain(|entry| entry != buffer);
        self.line_order.retain(|entry| entry != buffer);
        self.recent.retain(|entry| entry != buffer);
        self.alternate.retain(|_, entry| entry != buffer);
        if let Buffer::File(path) = buffer {
            self.unloaded.remove(path);
        }
    }

    /// Mark an already listed file as unloaded without changing its identity,
    /// ordering or per-window alternate references. The caller owns the
    /// lifetime and must first ensure no view or operation still needs it.
    pub fn mark_unloaded(&mut self, path: &Path) -> bool {
        if !self.listed.contains(&Buffer::File(path.to_path_buf())) {
            return false;
        }
        self.unloaded.insert(path.to_path_buf());
        true
    }

    pub fn is_unloaded(&self, path: &Path) -> bool {
        self.unloaded.contains(path)
    }

    pub fn mark_loaded(&mut self, path: &Path) {
        self.unloaded.remove(path);
    }

    /// Snacks first chooses a listed alternate local to each window, then
    /// falls back to the most recently used other listed buffer.
    pub fn replacement_for(&self, pane: PaneId, deleted: &Buffer) -> Option<Buffer> {
        self.alternate(pane, deleted)
            .filter(|entry| self.listed.contains(entry))
            .or_else(|| {
                self.recent
                    .iter()
                    .rev()
                    .find(|entry| *entry != deleted && self.listed.contains(entry))
                    .cloned()
            })
    }

    /// Plan all replacements before touching any view. Ignore attachments or
    /// files that have disappeared since they were last listed.
    pub fn plan_replacements(
        &self,
        panes: &[PaneId],
        deleted: &Buffer,
        available: impl Fn(&Buffer) -> bool,
    ) -> Option<BTreeMap<PaneId, Buffer>> {
        panes
            .iter()
            .map(|pane| {
                let alternate = self
                    .alternate(*pane, deleted)
                    .filter(|entry| self.listed.contains(entry))
                    .filter(|entry| available(entry));
                let fallback = || {
                    self.recent
                        .iter()
                        .rev()
                        .find(|entry| {
                            *entry != deleted && self.listed.contains(entry) && available(entry)
                        })
                        .cloned()
                };
                alternate.or_else(fallback).map(|entry| (*pane, entry))
            })
            .collect()
    }

    pub fn rebind(&mut self, old: &Buffer, new: Buffer) {
        if let (Buffer::File(old_path), Buffer::File(new_path)) = (old, &new)
            && self.unloaded.remove(old_path)
        {
            self.unloaded.insert(new_path.clone());
        }
        for entry in &mut self.listed {
            if entry == old {
                *entry = new.clone();
            }
        }
        for entry in &mut self.known {
            if entry == old {
                *entry = new.clone();
            }
        }
        for entry in &mut self.line_order {
            if entry == old {
                *entry = new.clone();
            }
        }
        for entry in self.alternate.values_mut() {
            if entry == old {
                *entry = new.clone();
            }
        }
        for entry in &mut self.recent {
            if entry == old {
                *entry = new.clone();
            }
        }
        let mut unique = Vec::new();
        for entry in self.listed.drain(..) {
            if !unique.contains(&entry) {
                unique.push(entry);
            }
        }
        self.listed = unique;
        let mut unique_known = Vec::new();
        for entry in self.known.drain(..) {
            if !unique_known.contains(&entry) {
                unique_known.push(entry);
            }
        }
        self.known = unique_known;
        let mut unique_line_order = Vec::new();
        for entry in self.line_order.drain(..) {
            if !unique_line_order.contains(&entry) {
                unique_line_order.push(entry);
            }
        }
        self.line_order = unique_line_order;
        let mut unique_recent = Vec::new();
        for entry in self.recent.drain(..).rev() {
            if !unique_recent.contains(&entry) {
                unique_recent.push(entry);
            }
        }
        unique_recent.reverse();
        self.recent = unique_recent;
    }

    /// Neovim :saveas keeps the current buffer identity at the new path and
    /// adds the old filename as the invoking window's alternate. The caller
    /// must arrange lazy loading of that filename before exposing this state.
    pub fn record_save_as(&mut self, pane: Option<PaneId>, old: &Buffer, new: Buffer) -> bool {
        if !matches!((old, &new), (Buffer::File(_), Buffer::File(_)))
            || old == &new
            || !self.known.contains(old)
            || self.known.contains(&new)
        {
            return false;
        }
        self.rebind(old, new.clone());
        self.register(old.clone());
        self.touch(&new);
        if let Buffer::File(path) = old {
            self.unloaded.insert(path.clone());
        }
        if let Some(pane) = pane {
            self.alternate.insert(pane, old.clone());
        }
        true
    }

    pub fn close_pane(&mut self, pane: PaneId) {
        self.alternate.remove(&pane);
    }

    /// A closed workspace discards its layout; future pane IDs must not
    /// inherit alternates belonging to the old windows.
    pub fn close_all_panes(&mut self) {
        self.alternate.clear();
    }

    pub fn duplicate_pane(&mut self, source: PaneId, target: PaneId) {
        if let Some(alternate) = self.alternate.get(&source).cloned() {
            self.alternate.insert(target, alternate);
        } else {
            self.alternate.remove(&target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_terminal_follows_activation_and_rebinding() {
        let mut registry = BufferRegistry::default();
        let file = Buffer::File("/synthetic/a".into());
        registry.register(Buffer::Terminal(77));
        registry.register(Buffer::Terminal(78));
        registry.register(file.clone());
        registry.touch(&Buffer::Terminal(77));
        registry.touch(&file);
        assert_eq!(registry.recent_terminal(|_| true), Some(77));
        assert_eq!(registry.recent_terminal(|id| id != 77), Some(78));
        registry.rebind(&Buffer::Terminal(77), Buffer::Terminal(79));
        assert_eq!(registry.recent_terminal(|_| true), Some(79));
        assert_eq!(registry.recent_terminal(|_| false), None);
    }
    #[test]
    fn vim_delete_requires_force_for_modified_and_never_discards_during_io() {
        assert_eq!(
            vim_delete_permission(true, false, false),
            VimDeletePermission::Modified
        );
        assert_eq!(
            vim_delete_permission(true, true, false),
            VimDeletePermission::Ready
        );
        assert_eq!(
            vim_delete_permission(true, true, true),
            VimDeletePermission::Busy
        );
        assert_eq!(
            vim_delete_permission(false, false, true),
            VimDeletePermission::Busy
        );
    }
    #[test]
    fn snacks_unlist_preserves_window_alternates_and_reopen_stays_unlisted() {
        // Clean Neovim 0.12.1 + local Snacks.bufdelete: two views of b,
        // alternate a; deletion shows a in both with b as each alternate.
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-snacks-unlisted.json"
        ))
        .unwrap();
        let a = Buffer::File("/synthetic/a".into());
        let b = Buffer::File("/synthetic/b".into());
        let c = Buffer::File("/synthetic/c".into());
        let number = |buffer: &Buffer| {
            if buffer == &a {
                reference["a"].as_u64().unwrap()
            } else if buffer == &b {
                reference["b"].as_u64().unwrap()
            } else {
                0
            }
        };
        let mut registry = BufferRegistry::default();
        registry.register(a.clone());
        registry.register(b.clone());
        registry.replace(PaneId(1), Some(&a), b.clone());
        registry.duplicate_pane(PaneId(1), PaneId(2));
        registry.record_replacements(&b, [(PaneId(1), a.clone()), (PaneId(2), a.clone())]);
        assert!(registry.unlist_file(Path::new("/synthetic/b")));
        assert!(registry.is_unloaded(Path::new("/synthetic/b")));
        assert!(registry.is_unlisted(&b));
        assert_eq!(registry.alternate(PaneId(1), &a), Some(b.clone()));
        assert_eq!(registry.alternate(PaneId(2), &a), Some(b.clone()));
        for (index, pane) in [PaneId(1), PaneId(2)].into_iter().enumerate() {
            assert_eq!(
                number(&a),
                reference["deleted"]["wins"][index]["cur"].as_u64().unwrap()
            );
            assert_eq!(
                number(&registry.alternate(pane, &a).unwrap()),
                reference["deleted"]["wins"][index]["alt"].as_u64().unwrap()
            );
        }
        assert_eq!(
            u64::from(!registry.is_unlisted(&b)),
            reference["deleted"]["bl"].as_u64().unwrap()
        );
        // An unlisted alternate must not be picked by Snacks as a fallback.
        assert_ne!(registry.replacement_for(PaneId(2), &a), Some(b.clone()));
        registry.mark_loaded(Path::new("/synthetic/b"));
        registry.replace(PaneId(1), Some(&a), b.clone());
        assert!(registry.is_unlisted(&b));
        assert_eq!(registry.alternate(PaneId(1), &b), Some(a.clone()));
        assert_eq!(registry.alternate(PaneId(2), &a), Some(b.clone()));
        assert_eq!(
            number(&registry.alternate(PaneId(1), &b).unwrap()),
            reference["restored"]["wins"][0]["alt"].as_u64().unwrap()
        );
        assert_eq!(
            number(&registry.cycle(&b, true).unwrap()),
            reference["nextstate"]["wins"][0]["cur"].as_u64().unwrap()
        );
        registry.register(c.clone());
        assert_eq!(registry.cycle(&b, true), Some(c.clone()));
        assert_eq!(registry.cycle(&b, false), Some(a.clone()));
        assert!(registry.relist(&b));
        assert_eq!(registry.listed(), &[a, b.clone(), c]);
        registry.remove(&b);
        assert!(!registry.is_unlisted(&b));
        assert_eq!(
            registry.alternate(PaneId(2), &Buffer::File("/synthetic/a".into())),
            None
        );
    }

    #[test]
    fn save_as_from_unlisted_keeps_current_unlisted_and_lists_old_name() {
        // Clean Neovim after Snacks deletion, :buffer #, :saveas: the renamed
        // current buffer stays unlisted; the old-name alternate is listed.
        let old = Buffer::File("/synthetic/old".into());
        let new = Buffer::File("/synthetic/new".into());
        let mut registry = BufferRegistry::default();
        registry.register(old.clone());
        assert!(registry.unlist_file(Path::new("/synthetic/old")));
        registry.mark_loaded(Path::new("/synthetic/old"));
        assert!(registry.record_save_as(Some(PaneId(1)), &old, new.clone()));
        assert!(registry.is_unlisted(&new));
        assert!(registry.listed().contains(&old));
        assert!(registry.is_unloaded(Path::new("/synthetic/old")));
        assert_eq!(registry.alternate(PaneId(1), &new), Some(old));
    }

    #[test]
    fn unlisting_shared_file_keeps_workspace_alternates_separate() {
        let old = Buffer::File("/synthetic/shared".into());
        let left = Buffer::File("/synthetic/left".into());
        let right = Buffer::Terminal(42);
        let mut workspace_a = BufferRegistry::default();
        let mut workspace_b = BufferRegistry::default();
        for (registry, replacement) in [
            (&mut workspace_a, left.clone()),
            (&mut workspace_b, right.clone()),
        ] {
            registry.register(old.clone());
            registry.register(replacement.clone());
            registry.record_replacements(&old, [(PaneId(1), replacement)]);
            assert!(registry.unlist_file(Path::new("/synthetic/shared")));
        }
        assert_eq!(workspace_a.alternate(PaneId(1), &left), Some(old.clone()));
        assert_eq!(workspace_b.alternate(PaneId(1), &right), Some(old.clone()));
        workspace_a.mark_loaded(Path::new("/synthetic/shared"));
        workspace_a.replace(PaneId(1), Some(&left), old.clone());
        assert!(workspace_a.is_unlisted(&old));
        assert!(workspace_b.is_unlisted(&old));
        assert_eq!(workspace_b.alternate(PaneId(1), &right), Some(old.clone()));
        assert!(workspace_a.relist(&old));
        assert!(!workspace_a.is_unlisted(&old));
        assert!(workspace_b.is_unlisted(&old));
    }

    #[test]
    fn tree_move_rebinds_unloaded_alternates_and_refuses_collisions() {
        let a = Buffer::File("/synthetic/a".into());
        let old = Buffer::File("/synthetic/old/b".into());
        let new = Buffer::File("/synthetic/new/b".into());
        let mut registry = BufferRegistry::default();
        registry.register(a.clone());
        registry.register(old.clone());
        registry.replace(PaneId(1), Some(&old), a.clone());
        assert!(registry.unlist_file(Path::new("/synthetic/old/b")));
        assert!(registry.move_unrepresented_files(
            Path::new("/synthetic/old"),
            Path::new("/synthetic/new"),
            |_| false
        ));
        assert!(registry.is_unlisted(&new));
        assert!(registry.is_unloaded(Path::new("/synthetic/new/b")));
        assert_eq!(registry.alternate(PaneId(1), &a), Some(new.clone()));

        let conflict = Buffer::File("/synthetic/target/b".into());
        registry.register(conflict.clone());
        assert!(!registry.move_unrepresented_files(
            Path::new("/synthetic/new"),
            Path::new("/synthetic/target"),
            |_| false
        ));
        assert!(registry.knows(&conflict));
        assert!(!registry.knows(&new));
        assert_eq!(registry.alternate(PaneId(1), &a), None);
    }
    #[test]
    fn closing_shared_file_detaches_only_the_current_workspace() {
        let file = PathBuf::from("shared.txt");
        let mut owners = HashMap::from([(file.clone(), BTreeSet::from([1, 2]))]);
        let mut last = HashMap::from([(1, file.clone()), (2, file.clone())]);
        assert!(detach_workspace_file(
            &file,
            1,
            &mut owners,
            &mut last,
            &BTreeSet::new()
        ));
        assert_eq!(owners[&file], BTreeSet::from([2]));
        assert_eq!(last.get(&2), Some(&file));
        assert!(!last.contains_key(&1));
        assert!(!detach_workspace_file(
            &file,
            2,
            &mut owners,
            &mut last,
            &BTreeSet::new()
        ));
        assert!(detach_workspace_file(
            &file,
            2,
            &mut owners,
            &mut last,
            &BTreeSet::from([3])
        ));
        assert_eq!(owners[&file], BTreeSet::from([3]));
    }
    #[test]
    fn forgetting_shared_file_preserves_replacement_ownership_and_other_workspace_history() {
        let mut owners = HashMap::from([
            (PathBuf::from("deleted"), BTreeSet::from([1, 2])),
            (PathBuf::from("replacement"), BTreeSet::from([1])),
            (PathBuf::from("other"), BTreeSet::from([2])),
        ]);
        let mut last = HashMap::from([
            (1, PathBuf::from("replacement")),
            (2, PathBuf::from("deleted")),
            (3, PathBuf::from("other")),
        ]);
        forget_deleted_file(Path::new("deleted"), &mut owners, &mut last);
        assert!(!owners.contains_key(Path::new("deleted")));
        assert_eq!(owners[Path::new("replacement")], BTreeSet::from([1]));
        assert_eq!(last.get(&1), Some(&PathBuf::from("replacement")));
        assert!(!last.contains_key(&2));
        assert_eq!(last.get(&3), Some(&PathBuf::from("other")));
    }
    #[test]
    fn fallback_is_a_live_file_from_the_same_workspace() {
        let mut workspace = BufferRegistry::default();
        workspace.register(Buffer::File("current".into()));
        workspace.register(Buffer::Terminal(3));
        workspace.register(Buffer::File("stale".into()));
        let other_workspace_file = Path::new("other-workspace");
        assert_eq!(
            workspace.fallback_file(Path::new("current"), |path| path == other_workspace_file),
            None
        );
        workspace.register(Buffer::File("local".into()));
        assert_eq!(
            workspace.fallback_file(Path::new("current"), |path| path == Path::new("local")),
            Some(PathBuf::from("local"))
        );
    }
    #[test]
    fn save_delete_requires_success_and_never_recovers_from_a_stale_or_failed_save() {
        let guard = DeleteGuard {
            path: "file".into(),
            project: 2,
            version: 9,
        };
        let mut pending = DeleteAfterSave::new(guard.clone());
        let allowed = |p: &DeleteAfterSave, version, dirty| {
            p.permits(Some(Path::new("file")), 2, Some(version), dirty)
        };
        assert!(!allowed(&pending, 9, false));
        pending.saved(true, 9, Some(9));
        assert!(allowed(&pending, 9, false));
        assert!(!allowed(&pending, 11, false)); // edited and undone: clean, but newer
        pending.formatted_version(10);
        assert!(allowed(&pending, 10, false));
        pending.formatting(false);
        pending.saved(true, 10, Some(10));
        assert!(!allowed(&pending, 10, false));
        let mut failed = DeleteAfterSave::new(guard);
        failed.saved(false, 9, Some(9));
        failed.saved(true, 9, Some(9));
        assert!(!allowed(&failed, 9, false));
        let mut stale = DeleteAfterSave::new(DeleteGuard {
            path: "file".into(),
            project: 2,
            version: 9,
        });
        stale.saved(true, 9, Some(11));
        assert!(!allowed(&stale, 11, false));
    }
    #[test]
    fn delete_confirmation_rejects_changed_target_version_or_inflight_save() {
        let guard = DeleteGuard {
            path: "file".into(),
            project: 2,
            version: 9,
        };
        assert!(guard.permits(Some(Path::new("file")), 2, Some(9), false));
        assert!(!guard.permits(Some(Path::new("other")), 2, Some(9), false));
        assert!(!guard.permits(Some(Path::new("file")), 3, Some(9), false));
        assert!(!guard.permits(Some(Path::new("file")), 2, Some(10), false));
        assert!(!guard.permits(Some(Path::new("file")), 2, Some(9), true));
        assert!(!guard.permits(None, 2, None, false));
    }

    #[test]
    fn listed_terminal_survives_replacement_and_cycles_with_files() {
        let mut list = BufferRegistry::default();
        let terminal = Buffer::Terminal(7);
        let file = Buffer::File("fixture.txt".into());
        list.register(terminal.clone());
        list.replace(PaneId(1), Some(&terminal), file.clone());
        assert_eq!(list.cycle(&file, false), Some(terminal.clone()));
        assert_eq!(list.cycle(&terminal, true), Some(file.clone()));
        assert_eq!(list.alternate(PaneId(1), &file), Some(terminal));
    }

    #[test]
    fn three_buffer_cycle_matches_clean_neovim_with_terminal_between_files() {
        // Clean Neovim, hidden enabled: :enew file-a, :terminal /bin/cat,
        // :enew file-b, then :bnext four times. No personal config or jobs.
        let file_a = Buffer::File(PathBuf::from("/synthetic-a"));
        let terminal = Buffer::Terminal(7);
        let file_b = Buffer::File(PathBuf::from("/synthetic-b"));
        let mut list = BufferRegistry::default();
        for buffer in [&file_a, &terminal, &file_b] {
            list.register(buffer.clone());
        }
        let mut current = file_b.clone();
        for expected in [&file_a, &terminal, &file_b, &file_a] {
            current = list.cycle(&current, true).unwrap();
            assert_eq!(&current, expected);
        }
    }
    #[test]
    fn terminal_normal_buffer_mapping_consumes_counts_once() {
        // Clean Neovim 0.12.1 with LazyVim's <Cmd>bnext/bprevious<CR> mappings:
        // L, 2L and 3L advance one listed buffer; H and 2H move back one.
        for (chord, initial, expected) in [
            ("L", 0, Some(true)),
            ("L", 2, Some(true)),
            ("L", 3, Some(true)),
            ("H", 2, Some(false)),
        ] {
            let mut count = initial;
            assert_eq!(normal_buffer_cycle(chord, false, &mut count), expected);
            assert_eq!(count, 0);
        }
        let mut count = 2;
        assert_eq!(normal_buffer_cycle("L", true, &mut count), None);
        assert_eq!(normal_buffer_cycle("j", false, &mut count), None);
        assert_eq!(count, 2);
    }
    #[test]
    fn cycling_skips_stale_file_and_terminal_without_losing_order() {
        let mut list = BufferRegistry::default();
        let current = Buffer::File("active".into());
        let stale_terminal = Buffer::Terminal(7);
        let stale_file = Buffer::File("gone".into());
        let live_terminal = Buffer::Terminal(8);
        for item in [&current, &stale_terminal, &stale_file, &live_terminal] {
            list.register(item.clone());
        }
        let available = |buffer: &Buffer| buffer == &current || buffer == &live_terminal;
        assert_eq!(
            list.cycle_available(&current, true, available),
            Some(live_terminal.clone())
        );
        assert_eq!(
            list.cycle_available(&current, false, available),
            Some(live_terminal.clone())
        );
        assert_eq!(
            list.cycle_available(&live_terminal, true, available),
            Some(current.clone())
        );
        assert_eq!(
            list.cycle_available(&current, true, |buffer| buffer == &current),
            None
        );
        assert_eq!(
            list.cycle_available(&Buffer::Terminal(99), true, |_| true),
            None
        );
    }

    #[test]
    fn alternate_is_per_window_and_does_not_resurrect_removed_buffers() {
        let mut list = BufferRegistry::default();
        let a = Buffer::File("a".into());
        let b = Buffer::File("b".into());
        let term = Buffer::Terminal(2);
        list.register(a.clone());
        list.register(term.clone());
        list.replace(PaneId(1), Some(&a), b.clone());
        list.replace(PaneId(2), Some(&term), b.clone());
        assert_eq!(list.alternate(PaneId(1), &b), Some(a.clone()));
        assert_eq!(list.alternate(PaneId(2), &b), Some(term.clone()));
        list.remove(&term);
        assert_eq!(list.alternate(PaneId(2), &b), None);
        assert_eq!(list.cycle(&b, true), Some(a));
    }

    #[test]
    fn reconnection_rebinds_terminal_identity_without_duplicate_entry() {
        let mut list = BufferRegistry::default();
        let old = Buffer::Terminal(8);
        let new = Buffer::Terminal(9);
        let file = Buffer::File("a".into());
        list.register(old.clone());
        list.register(new.clone());
        list.replace(PaneId(3), Some(&old), file.clone());
        list.rebind(&old, new.clone());
        assert_eq!(list.cycle(&file, false), Some(new.clone()));
        assert_eq!(list.alternate(PaneId(3), &file), Some(new));
        list.close_pane(PaneId(3));
        assert_eq!(list.alternate(PaneId(3), &file), None);
    }

    #[test]
    fn delete_uses_each_windows_alternate_then_most_recent_live_buffer() {
        let mut list = BufferRegistry::default();
        let terminal = Buffer::Terminal(1);
        let file = Buffer::File("deleted".into());
        let other = Buffer::File("other".into());
        list.register(terminal.clone());
        list.replace(PaneId(1), Some(&terminal), file.clone());
        list.register(other.clone());
        list.replace(PaneId(2), Some(&other), file.clone());
        assert_eq!(
            list.replacement_for(PaneId(1), &file),
            Some(terminal.clone())
        );
        assert_eq!(list.replacement_for(PaneId(2), &file), Some(other.clone()));
        assert_eq!(list.replacement_for(PaneId(3), &file), Some(other.clone()));
        list.remove(&other);
        assert_eq!(list.replacement_for(PaneId(2), &file), Some(terminal));
    }

    #[test]
    fn split_copies_alternate_and_close_forgets_only_the_closed_view() {
        let mut list = BufferRegistry::default();
        let terminal = Buffer::Terminal(1);
        let file = Buffer::File("a".into());
        list.register(terminal.clone());
        list.replace(PaneId(1), Some(&terminal), file.clone());
        list.duplicate_pane(PaneId(1), PaneId(2));
        list.close_pane(PaneId(1));
        assert_eq!(list.alternate(PaneId(1), &file), None);
        assert_eq!(list.alternate(PaneId(2), &file), Some(terminal));
    }

    #[test]
    fn save_as_rebinds_shared_buffer_and_sets_only_invoking_alternate() {
        let old = Buffer::File("/synthetic/source.txt".into());
        let new = Buffer::File("/synthetic/copy.txt".into());
        let (left, right) = (PaneId(1), PaneId(2));
        let mut registry = BufferRegistry::default();
        registry.register(old.clone());
        registry.duplicate_pane(left, right);
        assert!(registry.record_save_as(Some(right), &old, new.clone()));
        assert_eq!(registry.listed(), &[new.clone(), old.clone()]);
        assert_eq!(registry.alternate(right, &new), Some(old.clone()));
        assert_eq!(registry.alternate(left, &new), None);
        assert!(registry.is_unloaded(Path::new("/synthetic/source.txt")));
        registry.mark_loaded(Path::new("/synthetic/source.txt"));
        assert!(!registry.is_unloaded(Path::new("/synthetic/source.txt")));
        assert!(!registry.record_save_as(Some(right), &new, old.clone()));
        assert_eq!(registry.alternate(right, &new), Some(old));
    }

    #[test]
    fn unloading_listed_file_preserves_alternate_and_rejects_unlisted() {
        let old = Buffer::File("/synthetic/old.txt".into());
        let current = Buffer::File("/synthetic/current.txt".into());
        let mut registry = BufferRegistry::default();
        registry.register(old.clone());
        registry.replace(PaneId(1), Some(&old), current.clone());
        assert!(registry.mark_unloaded(Path::new("/synthetic/old.txt")));
        assert!(registry.is_unloaded(Path::new("/synthetic/old.txt")));
        assert_eq!(registry.alternate(PaneId(1), &current), Some(old.clone()));
        assert!(registry.listed().contains(&old));
        assert!(!registry.mark_unloaded(Path::new("/synthetic/missing.txt")));
        registry.mark_loaded(Path::new("/synthetic/old.txt"));
        assert!(!registry.is_unloaded(Path::new("/synthetic/old.txt")));
    }

    #[test]
    fn unloaded_state_tracks_rename_and_is_forgotten_when_removed() {
        let old = Buffer::File("/synthetic/old.txt".into());
        let new = Buffer::File("/synthetic/new.txt".into());
        let renamed = Buffer::File("/synthetic/renamed.txt".into());
        let mut registry = BufferRegistry::default();
        registry.register(old.clone());
        assert!(registry.record_save_as(Some(PaneId(1)), &old, new.clone()));
        assert_eq!(
            registry.cycle_available(
                &new,
                true,
                |item| matches!(item, Buffer::File(path) if registry.is_unloaded(path))
            ),
            Some(old.clone())
        );
        registry.rebind(&old, renamed.clone());
        assert!(!registry.is_unloaded(Path::new("/synthetic/old.txt")));
        assert!(registry.is_unloaded(Path::new("/synthetic/renamed.txt")));
        registry.remove(&renamed);
        assert!(!registry.is_unloaded(Path::new("/synthetic/renamed.txt")));
        assert_eq!(registry.alternate(PaneId(1), &new), None);
    }

    #[test]
    fn split_alternates_follow_independent_neovim_window_history() {
        // Clean Neovim: A -> terminal -> B -> :vsplit; new window -> A;
        // original remains B with terminal as alternate; :buffer # in the
        // new window returns to B and changes its alternate to A.
        let a = Buffer::File("/synthetic-a".into());
        let terminal = Buffer::Terminal(7);
        let b = Buffer::File("/synthetic-b".into());
        let (original, split) = (PaneId(1), PaneId(2));
        let mut registry = BufferRegistry::default();
        registry.register(a.clone());
        registry.replace(original, Some(&a), terminal.clone());
        registry.replace(original, Some(&terminal), b.clone());
        registry.duplicate_pane(original, split);
        assert_eq!(registry.alternate(split, &b), Some(terminal.clone()));
        registry.replace(split, Some(&b), a.clone());
        assert_eq!(registry.alternate(original, &b), Some(terminal));
        assert_eq!(registry.alternate(split, &a), Some(b.clone()));
        registry.replace(split, Some(&a), b.clone());
        assert_eq!(registry.alternate(split, &b), Some(a));
    }

    #[test]
    fn deletion_plan_skips_stale_targets_and_fails_without_a_live_fallback() {
        let mut list = BufferRegistry::default();
        let stale = Buffer::Terminal(1);
        let live = Buffer::Terminal(2);
        let file = Buffer::File("a".into());
        list.register(stale.clone());
        list.register(live.clone());
        list.replace(PaneId(1), Some(&stale), file.clone());
        let plan = list
            .plan_replacements(&[PaneId(1), PaneId(2)], &file, |item| *item != stale)
            .unwrap();
        assert_eq!(plan[&PaneId(1)], live);
        assert_eq!(plan[&PaneId(2)], live);
        assert!(
            list.plan_replacements(&[PaneId(1)], &file, |_| false)
                .is_none()
        );
    }
}
