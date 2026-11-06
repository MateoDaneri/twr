//! Stable buffer identities, independent of listed order and window focus.
//!
//! This catalog does not decide whether a buffer is loaded, listed in a
//! workspace, or safe to discard. The caller owns those decisions. In
//! particular, unlisting is not a wipe and must leave the number intact.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::buffer_registry::{Buffer, BufferRegistry};
use workspace_core::editor_panes::PaneId;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Missing,
    Conflict,
    Exhausted,
    NotFile,
    Shared,
}

pub struct BufferNumbers {
    next: u64,
    by_number: BTreeMap<u64, Buffer>,
    by_buffer: BTreeMap<Buffer, u64>,
    owners: BTreeMap<u64, BTreeSet<usize>>,
}

impl Default for BufferNumbers {
    fn default() -> Self {
        Self {
            next: 1,
            by_number: BTreeMap::new(),
            by_buffer: BTreeMap::new(),
            owners: BTreeMap::new(),
        }
    }
}

impl BufferNumbers {
    pub fn get(&self, number: u64) -> Option<&Buffer> {
        self.by_number.get(&number)
    }

    pub fn number(&self, buffer: &Buffer) -> Option<u64> {
        self.by_buffer.get(buffer).copied()
    }

    /// A tree move that only rewrites live editors cannot safely update an
    /// unloaded or hidden name under the same directory.
    pub fn has_unrepresented_file_under(
        &self,
        root: &Path,
        represented: impl Fn(&Path) -> bool,
    ) -> bool {
        self.by_buffer.keys().any(|buffer| match buffer {
            Buffer::File(path) => path.starts_with(root) && !represented(path),
            Buffer::Terminal(_) | Buffer::Explorer(_) | Buffer::Agents(_) => false,
        })
    }

    /// Record that a workspace knows this buffer. This does not list or load it.
    pub fn grant(&mut self, workspace: usize, buffer: Buffer) -> Result<u64, Error> {
        let number = self.ensure(buffer)?;
        self.owners.entry(number).or_default().insert(workspace);
        Ok(number)
    }

    /// Numeric lookup never exposes a buffer from another workspace.
    pub fn get_for(&self, workspace: usize, number: u64) -> Option<&Buffer> {
        self.owners
            .get(&number)
            .filter(|owners| owners.contains(&workspace))
            .and_then(|_| self.get(number))
    }

    /// Closing a workspace removes its access, not other owners or the ID.
    pub fn revoke(&mut self, workspace: usize, number: u64) -> bool {
        let Some(owners) = self.owners.get_mut(&number) else {
            return false;
        };
        let removed = owners.remove(&workspace);
        if owners.is_empty() {
            self.owners.remove(&number);
        }
        removed
    }

    pub fn revoke_workspace(&mut self, workspace: usize) {
        self.owners.retain(|_, owners| {
            owners.remove(&workspace);
            !owners.is_empty()
        });
    }

    /// As with bufadd(), finding an existing unlisted identity preserves its ID.
    pub fn ensure(&mut self, buffer: Buffer) -> Result<u64, Error> {
        if let Some(number) = self.number(&buffer) {
            return Ok(number);
        }
        let following = self.next.checked_add(1).ok_or(Error::Exhausted)?;
        let number = self.next;
        self.by_number.insert(number, buffer.clone());
        self.by_buffer.insert(buffer, number);
        self.next = following;
        Ok(number)
    }

    /// Change an identity without changing its number, rejecting collisions.
    pub fn rebind(&mut self, number: u64, new: Buffer) -> Result<(), Error> {
        let old = self.get(number).cloned().ok_or(Error::Missing)?;
        if old == new {
            return Ok(());
        }
        if self.by_buffer.contains_key(&new) {
            return Err(Error::Conflict);
        }
        self.by_buffer.remove(&old);
        self.by_buffer.insert(new.clone(), number);
        self.by_number.insert(number, new);
        Ok(())
    }

    /// Model a named :file rename: keep the number and retain the old name as
    /// a separate, unlisted identity. Listing and loading remain caller-owned.
    pub fn rename_file(&mut self, number: u64, new: Buffer) -> Result<u64, Error> {
        let old = self.get(number).cloned().ok_or(Error::Missing)?;
        if !matches!((&old, &new), (Buffer::File(_), Buffer::File(_))) {
            return Err(Error::NotFile);
        }
        if old == new || self.by_buffer.contains_key(&new) {
            return Err(Error::Conflict);
        }
        // Preflight allocation before changing either index.
        let following = self.next.checked_add(1).ok_or(Error::Exhausted)?;
        let old_name_number = self.next;
        self.by_number.insert(number, new.clone());
        self.by_buffer.insert(new, number);
        self.by_buffer.insert(old.clone(), old_name_number);
        self.by_number.insert(old_name_number, old);
        self.next = following;
        Ok(old_name_number)
    }

    /// A shared buffer keeps all owners after rename; the old-name alternate
    /// belongs only to the workspace that performed the operation.
    pub fn rename_file_for(
        &mut self,
        workspace: usize,
        number: u64,
        new: Buffer,
    ) -> Result<u64, Error> {
        if self.get_for(workspace, number).is_none() {
            return Err(Error::Missing);
        }
        let old_name_number = self.rename_file(number, new)?;
        self.owners
            .entry(old_name_number)
            .or_default()
            .insert(workspace);
        Ok(old_name_number)
    }

    /// Wiping destroys the identity. Numbers are never reused within a catalog.
    pub fn wipe(&mut self, number: u64) -> Option<Buffer> {
        let buffer = self.by_number.remove(&number)?;
        self.by_buffer.remove(&buffer);
        self.owners.remove(&number);
        Some(buffer)
    }

    /// A workspace cannot destroy an identity still owned by another one.
    pub fn wipe_for(&mut self, workspace: usize, number: u64) -> Result<Buffer, Error> {
        let owners = self.owners.get(&number).ok_or(Error::Missing)?;
        if !owners.contains(&workspace) {
            return Err(Error::Missing);
        }
        if owners.iter().any(|owner| *owner != workspace) {
            return Err(Error::Shared);
        }
        self.wipe(number).ok_or(Error::Missing)
    }
}

/// Shadow index for the live UI while lifecycle integration is incomplete.
/// An unrepresentable transition disables it; it is never reconstructed from
/// current list order because that would assign misleading historical numbers.
pub struct BufferTracking(Option<BufferNumbers>);

#[derive(Debug, PartialEq, Eq)]
pub enum LookupError {
    Unavailable,
    Missing,
}

/// Data for one :ls row; availability and workspace ownership are checked by
/// the caller before constructing it.
pub struct ListingRow {
    pub number: u64,
    pub name: String,
    pub listed: bool,
    pub current: bool,
    pub alternate: bool,
    pub visible: bool,
    pub loaded: bool,
    pub modified: bool,
    pub line: usize,
}

pub fn format_listing(rows: &[ListingRow]) -> String {
    rows.iter()
        .map(|row| {
            let unlisted = if row.listed { ' ' } else { 'u' };
            let current = if row.current {
                '%'
            } else if row.alternate {
                '#'
            } else {
                ' '
            };
            let activity = if row.visible {
                'a'
            } else if row.loaded {
                'h'
            } else {
                ' '
            };
            let modified = if row.modified { '+' } else { ' ' };
            format!(
                "{:>3}{unlisted}{current}{activity} {modified} {:?} line {}",
                row.number, row.name, row.line
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Default for BufferTracking {
    fn default() -> Self {
        Self(Some(BufferNumbers::default()))
    }
}

impl BufferTracking {
    /// Remove a wiped identity only when this workspace owns it exclusively.
    /// Disabled tracking has no numeric identity to remove.
    pub fn wipe_owned(&mut self, workspace: usize, buffer: &Buffer) -> bool {
        let Some(numbers) = &mut self.0 else {
            return true;
        };
        let Some(number) = numbers.number(buffer) else {
            return false;
        };
        numbers.wipe_for(workspace, number).as_ref() == Ok(buffer)
    }
    /// Display only numbers that would resolve to the same identity in this
    /// workspace. An invalidated tracker must never invent a list position.
    pub fn number_for(&self, workspace: usize, buffer: &Buffer) -> Result<u64, LookupError> {
        let numbers = self.0.as_ref().ok_or(LookupError::Unavailable)?;
        let number = numbers.number(buffer).ok_or(LookupError::Missing)?;
        if numbers.get_for(workspace, number) != Some(buffer) {
            return Err(LookupError::Missing);
        }
        Ok(number)
    }
    pub fn resolve(&self, workspace: usize, number: u64) -> Result<Buffer, LookupError> {
        self.0
            .as_ref()
            .ok_or(LookupError::Unavailable)?
            .get_for(workspace, number)
            .cloned()
            .ok_or(LookupError::Missing)
    }
    pub fn register(&mut self, workspace: usize, buffer: Buffer) {
        if self
            .0
            .as_mut()
            .is_some_and(|numbers| numbers.grant(workspace, buffer).is_err())
        {
            self.invalidate();
        }
    }

    pub fn rebind(&mut self, old: &Buffer, new: Buffer, save_as_owner: Option<usize>) {
        let failed = self.0.as_mut().is_some_and(|numbers| {
            let Some(number) = numbers.number(old) else {
                return true;
            };
            if let Some(owner) = save_as_owner {
                numbers.rename_file_for(owner, number, new).is_err()
            } else {
                numbers.rebind(number, new).is_err()
            }
        });
        if failed {
            self.invalidate();
        }
    }

    pub fn revoke_workspace(&mut self, workspace: usize) {
        if let Some(numbers) = &mut self.0 {
            numbers.revoke_workspace(workspace);
        }
    }

    pub fn tree_move(&mut self, root: &Path, represented: impl Fn(&Path) -> bool) {
        if self
            .0
            .as_ref()
            .is_some_and(|numbers| numbers.has_unrepresented_file_under(root, represented))
        {
            self.invalidate();
        }
    }

    pub fn invalidate(&mut self) {
        self.0 = None;
    }
}

/// One number namespace, with independent listed buffers for each workspace.
/// This does not own editor instances, terminal processes, or pending I/O.
#[derive(Default)]
pub struct WorkspaceBuffers {
    numbers: BufferNumbers,
    registries: BTreeMap<usize, BufferRegistry>,
}

impl WorkspaceBuffers {
    pub fn register(&mut self, workspace: usize, buffer: Buffer) -> Result<u64, Error> {
        let number = self.numbers.grant(workspace, buffer.clone())?;
        self.registries
            .entry(workspace)
            .or_default()
            .register(buffer);
        Ok(number)
    }

    pub fn resolve(&self, workspace: usize, number: u64) -> Option<&Buffer> {
        self.numbers.get_for(workspace, number)
    }

    pub fn registry(&self, workspace: usize) -> Option<&BufferRegistry> {
        self.registries.get(&workspace)
    }

    /// Unlisting is not a wipe: the owner can still resolve the same number.
    pub fn unlist(&mut self, workspace: usize, number: u64) -> bool {
        let Some(buffer) = self.resolve(workspace, number).cloned() else {
            return false;
        };
        let Some(registry) = self.registries.get_mut(&workspace) else {
            return false;
        };
        if !registry.listed().contains(&buffer) {
            return false;
        }
        registry.remove(&buffer);
        true
    }

    pub fn close_workspace(&mut self, workspace: usize) {
        self.registries.remove(&workspace);
        self.numbers.revoke_workspace(workspace);
    }

    /// A successful :saveas keeps the active ID and makes the old filename a
    /// listed, unloaded alternate only for the invoking workspace. Preflight
    /// registry changes on clones before changing the shared number catalog.
    pub fn record_save_as(
        &mut self,
        workspace: usize,
        pane: Option<PaneId>,
        number: u64,
        new: Buffer,
    ) -> Result<u64, Error> {
        let old = self
            .resolve(workspace, number)
            .cloned()
            .ok_or(Error::Missing)?;
        if !matches!((&old, &new), (Buffer::File(_), Buffer::File(_))) {
            return Err(Error::NotFile);
        }
        if old == new || self.numbers.number(&new).is_some() {
            return Err(Error::Conflict);
        }
        let mut staged = self.registries.clone();
        let invoking = staged.get_mut(&workspace).ok_or(Error::Missing)?;
        if !invoking.record_save_as(pane, &old, new.clone()) {
            return Err(Error::Conflict);
        }
        for (project, registry) in &mut staged {
            if *project != workspace {
                registry.rebind(&old, new.clone());
            }
        }
        let old_name = self.numbers.rename_file_for(workspace, number, new)?;
        self.registries = staged;
        Ok(old_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_wipe_removes_only_exclusive_numeric_identity() {
        let old = Buffer::Terminal(41);
        let replacement = Buffer::Terminal(42);
        let mut tracking = BufferTracking::default();
        tracking.register(1, old.clone());
        let number = tracking.number_for(1, &old).unwrap();
        tracking.register(2, old.clone());
        assert!(!tracking.wipe_owned(1, &old));
        assert_eq!(tracking.resolve(1, number), Ok(old.clone()));
        assert_eq!(tracking.resolve(2, number), Ok(old.clone()));
        tracking.revoke_workspace(2);
        assert!(tracking.wipe_owned(1, &old));
        assert_eq!(tracking.resolve(1, number), Err(LookupError::Missing));
        tracking.register(1, replacement.clone());
        assert_ne!(tracking.number_for(1, &replacement).unwrap(), number);
    }

    #[test]
    fn listing_flags_match_clean_neovim_for_named_file_examples() {
        let rows = vec![
            ListingRow {
                number: 1,
                name: "/private/tmp/twr-ls-reference/one.txt".into(),
                listed: true,
                current: false,
                alternate: true,
                visible: false,
                loaded: true,
                modified: false,
                line: 1,
            },
            ListingRow {
                number: 2,
                name: "/private/tmp/twr-ls-reference/two.txt".into(),
                listed: true,
                current: true,
                alternate: false,
                visible: true,
                loaded: true,
                modified: true,
                line: 1,
            },
        ];
        assert_eq!(
            format_listing(&rows),
            "  1 #h   \"/private/tmp/twr-ls-reference/one.txt\" line 1\n  2 %a + \"/private/tmp/twr-ls-reference/two.txt\" line 1"
        );
        let mut unlisted = rows;
        unlisted[0].listed = false;
        unlisted[0].loaded = false;
        assert!(
            format_listing(&unlisted)
                .starts_with("  1u#    \"/private/tmp/twr-ls-reference/one.txt\" line 1")
        );
    }

    #[test]
    fn named_ids_follow_clean_neovim_lifecycle_reference() {
        let r: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-numbered-buffer-reference.json"
        ))
        .unwrap();
        let n = |path: &[&str]| {
            path.iter()
                .fold(&r, |value, key| &value[*key])
                .as_u64()
                .unwrap()
        };
        let one = Buffer::File("one.txt".into());
        let two = Buffer::File("two.txt".into());
        let three = Buffer::File("three.txt".into());
        let renamed = Buffer::File("renamed.txt".into());
        let mut ids = BufferNumbers::default();
        assert_eq!(ids.ensure(one).unwrap(), n(&["created", "one"]));
        let second = ids.ensure(two.clone()).unwrap();
        assert_eq!(second, n(&["created", "two"]));
        assert_eq!(ids.ensure(three).unwrap(), n(&["created", "three"]));
        // Unlisting is managed elsewhere: the catalog still finds the ID.
        assert_eq!(ids.ensure(two.clone()).unwrap(), n(&["readd", "id"]));
        let old_name = ids.rename_file(second, renamed.clone()).unwrap();
        assert_eq!(ids.number(&renamed), Some(n(&["renamed", "new"])));
        assert_eq!(old_name, n(&["renamed", "old"]));
        assert_eq!(ids.get(old_name), Some(&two));
        assert_eq!(ids.wipe(second), Some(renamed.clone()));
        assert!(ids.get(second).is_none());
        assert_eq!(ids.ensure(renamed).unwrap(), n(&["after_wipe", "id"]));
    }

    #[test]
    fn collision_and_exhaustion_leave_both_indexes_unchanged() {
        let mut ids = BufferNumbers::default();
        let a = Buffer::File("a".into());
        let b = Buffer::File("b".into());
        let first = ids.ensure(a.clone()).unwrap();
        let second = ids.ensure(b.clone()).unwrap();
        assert_eq!(ids.rename_file(first, b.clone()), Err(Error::Conflict));
        assert_eq!(ids.rebind(first, b.clone()), Err(Error::Conflict));
        assert_eq!(ids.get(first), Some(&a));
        assert_eq!(ids.number(&b), Some(second));
        ids.next = u64::MAX;
        assert_eq!(
            ids.rename_file(first, Buffer::File("c".into())),
            Err(Error::Exhausted)
        );
        assert_eq!(ids.get(first), Some(&a));
        assert_eq!(ids.number(&a), Some(first));
        assert_eq!(ids.ensure(Buffer::File("c".into())), Err(Error::Exhausted));
    }

    #[test]
    fn reconnection_rebinds_terminal_and_wiping_does_not_reuse_its_number() {
        let mut ids = BufferNumbers::default();
        let old = Buffer::Terminal(4);
        let new = Buffer::Terminal(9);
        let number = ids.grant(7, old.clone()).unwrap();
        assert_eq!(ids.rebind(number, new.clone()), Ok(()));
        assert_eq!(ids.number(&old), None);
        assert_eq!(ids.number(&new), Some(number));
        assert_eq!(ids.get(number), Some(&new));
        assert_eq!(ids.get_for(7, number), Some(&new));
        assert_eq!(ids.get_for(8, number), None);
        assert_eq!(ids.wipe_for(7, number), Ok(new.clone()));
        assert_ne!(ids.ensure(new).unwrap(), number);
    }

    #[test]
    fn workspace_grants_follow_shared_identity_without_leaking_other_buffers() {
        let mut ids = BufferNumbers::default();
        let shared = Buffer::File("shared".into());
        let private = Buffer::File("private".into());
        let shared_number = ids.grant(1, shared.clone()).unwrap();
        assert_eq!(ids.grant(2, shared.clone()).unwrap(), shared_number);
        let private_number = ids.grant(1, private.clone()).unwrap();
        assert_eq!(ids.get_for(1, shared_number), Some(&shared));
        assert_eq!(ids.get_for(2, shared_number), Some(&shared));
        assert_eq!(ids.get_for(2, private_number), None);
        assert_eq!(ids.wipe_for(1, shared_number), Err(Error::Shared));
        assert_eq!(ids.wipe_for(2, private_number), Err(Error::Missing));
        assert!(ids.revoke(1, shared_number));
        assert_eq!(ids.get_for(1, shared_number), None);
        assert_eq!(ids.get_for(2, shared_number), Some(&shared));
        assert_eq!(ids.grant(1, shared.clone()).unwrap(), shared_number);
        assert_eq!(ids.wipe_for(2, shared_number), Err(Error::Shared));
        assert!(ids.revoke(1, shared_number));
        assert_eq!(ids.wipe_for(2, shared_number), Ok(shared.clone()));
        assert_eq!(ids.get_for(1, shared_number), None);
        assert_eq!(ids.get_for(2, shared_number), None);
        assert_ne!(ids.grant(2, shared).unwrap(), shared_number);
        assert_eq!(ids.get_for(1, private_number), Some(&private));
    }

    #[test]
    fn rename_preserves_shared_identity_and_scopes_old_name_to_invoker() {
        let mut ids = BufferNumbers::default();
        let old = Buffer::File("old".into());
        let new = Buffer::File("new".into());
        let number = ids.grant(1, old.clone()).unwrap();
        assert_eq!(ids.grant(2, old.clone()).unwrap(), number);
        assert_eq!(
            ids.rename_file_for(3, number, new.clone()),
            Err(Error::Missing)
        );
        assert_eq!(ids.get_for(1, number), Some(&old));
        let alternate = ids.rename_file_for(1, number, new.clone()).unwrap();
        assert_eq!(ids.get_for(1, number), Some(&new));
        assert_eq!(ids.get_for(2, number), Some(&new));
        assert_eq!(ids.get_for(1, alternate), Some(&old));
        assert_eq!(ids.get_for(2, alternate), None);
        ids.revoke_workspace(1);
        assert_eq!(ids.get_for(1, number), None);
        assert_eq!(ids.get_for(2, number), Some(&new));
        assert_eq!(ids.get_for(1, alternate), None);
        assert_eq!(ids.number(&old), Some(alternate));
    }

    #[test]
    fn directory_separates_unlisting_from_identity_and_workspace_access() {
        let mut directory = WorkspaceBuffers::default();
        let file = Buffer::File("shared".into());
        let number = directory.register(1, file.clone()).unwrap();
        assert_eq!(directory.register(2, file.clone()).unwrap(), number);
        assert!(directory.unlist(1, number));
        assert!(directory.registry(1).unwrap().listed().is_empty());
        assert_eq!(directory.resolve(1, number), Some(&file));
        assert_eq!(
            directory.registry(2).unwrap().listed(),
            std::slice::from_ref(&file)
        );
        assert_eq!(directory.resolve(3, number), None);
        directory.close_workspace(1);
        assert_eq!(directory.resolve(1, number), None);
        assert_eq!(directory.resolve(2, number), Some(&file));
        assert_eq!(directory.register(1, file).unwrap(), number);
    }

    #[test]
    fn save_as_preserves_shared_number_and_lists_old_name_only_for_invoker() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-buffer-saveas-reference.json"
        ))
        .unwrap();
        let mut directory = WorkspaceBuffers::default();
        let old = Buffer::File("old".into());
        let new = Buffer::File("new".into());
        let current = directory.register(1, old.clone()).unwrap();
        assert_eq!(directory.register(2, old.clone()).unwrap(), current);
        let pane = PaneId(7);
        let alternate = directory
            .record_save_as(1, Some(pane), current, new.clone())
            .unwrap();
        assert_eq!(current, reference["after"].as_u64().unwrap());
        assert_eq!(alternate, reference["old"].as_u64().unwrap());
        assert_eq!(reference["old_listed"], 1);
        assert_eq!(reference["old_loaded"], 0);
        assert_eq!(directory.resolve(1, current), Some(&new));
        assert_eq!(directory.resolve(2, current), Some(&new));
        assert_eq!(directory.resolve(1, alternate), Some(&old));
        assert_eq!(directory.resolve(2, alternate), None);
        let invoking = directory.registry(1).unwrap();
        assert!(invoking.listed().contains(&old));
        assert!(invoking.is_unloaded(std::path::Path::new("old")));
        assert_eq!(invoking.alternate(pane, &new), Some(old));
        assert_eq!(directory.registry(2).unwrap().listed(), &[new]);
    }

    #[test]
    fn failed_save_as_never_changes_registry_or_numbers() {
        let mut directory = WorkspaceBuffers::default();
        let old = Buffer::File("old".into());
        let occupied = Buffer::File("occupied".into());
        let current = directory.register(1, old.clone()).unwrap();
        let other = directory.register(2, occupied.clone()).unwrap();
        assert_eq!(
            directory.record_save_as(1, None, current, occupied.clone()),
            Err(Error::Conflict)
        );
        assert_eq!(directory.resolve(1, current), Some(&old));
        assert_eq!(directory.resolve(2, other), Some(&occupied));
        assert_eq!(directory.registry(1).unwrap().listed(), &[old]);
    }

    #[test]
    fn tree_move_detects_hidden_names_without_confusing_sibling_paths() {
        let mut numbers = BufferNumbers::default();
        numbers
            .grant(1, Buffer::File("root/live.txt".into()))
            .unwrap();
        numbers
            .grant(1, Buffer::File("root/hidden.txt".into()))
            .unwrap();
        numbers
            .grant(1, Buffer::File("root-sibling/file".into()))
            .unwrap();
        numbers.grant(1, Buffer::Terminal(2)).unwrap();
        assert!(
            numbers.has_unrepresented_file_under(Path::new("root"), |path| path
                == Path::new("root/live.txt"))
        );
        assert!(!numbers.has_unrepresented_file_under(Path::new("root"), |_| true));
        assert!(!numbers.has_unrepresented_file_under(Path::new("other"), |_| false));
    }

    #[test]
    fn shadow_tracking_invalidates_instead_of_reassigning_an_uncertain_identity() {
        let old = Buffer::File("root/old".into());
        let target = Buffer::File("root/target".into());
        let mut tracking = BufferTracking::default();
        tracking.register(1, old.clone());
        tracking.register(1, target.clone());
        tracking.rebind(&old, target.clone(), None);
        assert!(tracking.0.is_none());
        tracking.register(1, old.clone());
        assert!(tracking.0.is_none());

        let mut tracking = BufferTracking::default();
        tracking.register(1, old.clone());
        tracking.rebind(&old, target.clone(), None);
        assert_eq!(tracking.0.as_ref().unwrap().get_for(1, 1), Some(&target));
        tracking.tree_move(Path::new("root"), |_| false);
        assert!(tracking.0.is_none());
    }

    #[test]
    fn shadow_lookup_is_scoped_and_fails_closed_after_invalidation() {
        let mut tracking = BufferTracking::default();
        let file = Buffer::File("synthetic".into());
        tracking.register(1, file.clone());
        assert_eq!(tracking.number_for(1, &file), Ok(1));
        assert_eq!(tracking.number_for(2, &file), Err(LookupError::Missing));
        assert_eq!(tracking.resolve(1, 1), Ok(file));
        assert_eq!(tracking.resolve(2, 1), Err(LookupError::Missing));
        assert_eq!(tracking.resolve(1, 2), Err(LookupError::Missing));
        tracking.invalidate();
        assert_eq!(tracking.resolve(1, 1), Err(LookupError::Unavailable));
        assert_eq!(
            tracking.number_for(1, &Buffer::File("synthetic".into())),
            Err(LookupError::Unavailable)
        );
    }

    #[test]
    fn unnamed_unlist_and_empty_reopen_follow_neovim_reference() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-unnamed-reopen-reference.json"
        ))
        .unwrap();
        let project = 7;
        let pane = PaneId(1);
        let original = Buffer::File("/synthetic/original".into());
        // This is an identity token only; the test never opens the path.
        let path = std::path::PathBuf::from("/synthetic/unnamed-token");
        let unnamed = Buffer::File(path.clone());
        let mut tracking = BufferTracking::default();
        let mut registry = BufferRegistry::default();
        for buffer in [&original, &unnamed] {
            tracking.register(project, buffer.clone());
            registry.register(buffer.clone());
        }
        registry.replace(pane, Some(&original), unnamed.clone());
        registry.record_replacements(&unnamed, [(pane, original.clone())]);
        assert!(registry.unlist_file(&path));
        assert_eq!(
            tracking.number_for(project, &unnamed).unwrap(),
            reference["id"].as_u64().unwrap()
        );
        assert_eq!(
            tracking
                .resolve(project, reference["id"].as_u64().unwrap())
                .unwrap(),
            unnamed
        );
        assert_eq!(
            u64::from(registry.is_unloaded(&path)),
            1 - reference["before"]["loaded"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(registry.is_unlisted(&unnamed)),
            1 - reference["before"]["listed"].as_u64().unwrap()
        );
        assert_eq!(registry.alternate(pane, &original), Some(unnamed.clone()));
        let reopened = crate::editor::Editor::new(String::new());
        registry.mark_loaded(&path);
        registry.replace(pane, Some(&original), unnamed.clone());
        assert!(!registry.is_unloaded(&path));
        assert!(registry.is_unlisted(&unnamed));
        assert_eq!(
            reopened.contents().split('\n').collect::<Vec<_>>(),
            vec![reference["lines"][0].as_str().unwrap()]
        );
    }

    #[test]
    fn unnamed_saveas_keeps_id_and_creates_scoped_unnamed_alternate() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../Resources/Fixtures/nvim-unnamed-saveas-reference.json"
        ))
        .unwrap();
        let owner = 1;
        let other = 2;
        let pane = PaneId(1);
        let unnamed = Buffer::File("/synthetic/unnamed-token".into());
        let named = Buffer::File("/synthetic/new.txt".into());
        let mut tracking = BufferTracking::default();
        let mut owner_registry = BufferRegistry::default();
        let mut other_registry = BufferRegistry::default();
        for (project, registry) in [(owner, &mut owner_registry), (other, &mut other_registry)] {
            tracking.register(project, unnamed.clone());
            registry.register(unnamed.clone());
        }
        tracking.rebind(&unnamed, named.clone(), Some(owner));
        assert!(owner_registry.record_save_as(Some(pane), &unnamed, named.clone()));
        other_registry.rebind(&unnamed, named.clone());
        assert_eq!(
            tracking.number_for(owner, &named).unwrap(),
            reference["after"].as_u64().unwrap()
        );
        assert_eq!(
            tracking.number_for(other, &named).unwrap(),
            reference["after"].as_u64().unwrap()
        );
        assert_eq!(
            tracking.number_for(owner, &unnamed).unwrap(),
            reference["alternate"].as_u64().unwrap()
        );
        assert_eq!(
            tracking.number_for(other, &unnamed),
            Err(LookupError::Missing)
        );
        assert_eq!(
            owner_registry.alternate(pane, &named),
            Some(unnamed.clone())
        );
        assert_eq!(
            u64::from(owner_registry.listed().contains(&unnamed)),
            reference["alternate_listed"].as_u64().unwrap()
        );
        if let Buffer::File(path) = &unnamed {
            assert_eq!(
                u64::from(!owner_registry.is_unloaded(path)),
                reference["alternate_loaded"].as_u64().unwrap()
            );
        } else {
            unreachable!();
        }
        assert!(!other_registry.knows(&unnamed));
    }

    #[test]
    fn shadow_tracks_registration_save_as_and_terminal_reconnect_across_workspaces() {
        let mut tracking = BufferTracking::default();
        let original = Buffer::File("old".into());
        let renamed = Buffer::File("new".into());
        tracking.register(1, original.clone());
        tracking.register(2, original.clone());
        tracking.rebind(&original, renamed.clone(), Some(1));
        let numbers = tracking.0.as_ref().unwrap();
        assert_eq!(numbers.get_for(1, 1), Some(&renamed));
        assert_eq!(numbers.get_for(2, 1), Some(&renamed));
        assert_eq!(numbers.get_for(1, 2), Some(&original));
        assert_eq!(numbers.get_for(2, 2), None);
        tracking.register(1, Buffer::Terminal(40));
        tracking.rebind(&Buffer::Terminal(40), Buffer::Terminal(41), None);
        assert_eq!(
            tracking.0.as_ref().unwrap().get_for(1, 3),
            Some(&Buffer::Terminal(41))
        );
        tracking.revoke_workspace(1);
        let numbers = tracking.0.as_ref().unwrap();
        assert_eq!(numbers.get_for(1, 1), None);
        assert_eq!(numbers.get_for(2, 1), Some(&renamed));
    }
}
