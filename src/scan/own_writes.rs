//! Whether a subvolume changed during a scan only through the tool's own
//! state directory (SPEC 22.4): then its generation change is
//! self-inflicted and cannot have altered what the scan measured.

use crate::btrfs::find_new::{changed_inodes, parents};
use crate::btrfs::source::MetadataSource;
use crate::btrfs::subvolume::SubvolumeSet;
use crate::btrfs::walk::{BatchBoundary, WalkError};
use crate::report::{ChangeReason, StateDirWrites};
use std::collections::{BTreeMap, BTreeSet, HashSet};

/// Tracing more inodes than this for one changed inode fails closed.
const MAX_DEPTH: usize = 4096;

/// More changed inodes than this in one subvolume keeps its change.
const MAX_CHANGED_INODES: usize = 1 << 16;

/// Display-only inode ownership proof. This does not certify a generation
/// change or any cleanup operation. The bounded memo is local to one scan.
#[derive(Debug, Default)]
pub(super) struct StateOwnership {
    pub roots: BTreeSet<(u64, u64)>,
    decisions: BTreeMap<(u64, u64), bool>,
    lookups: usize,
}

impl StateOwnership {
    pub fn owns<S: MetadataSource + ?Sized>(
        &mut self,
        source: &mut S,
        tree: u64,
        inode: u64,
        boundary: &mut dyn BatchBoundary,
    ) -> Result<bool, WalkError> {
        if !self.roots.iter().any(|r| r.0 == tree) {
            return Ok(false);
        }
        if let Some(&found) = self.decisions.get(&(tree, inode)) {
            return Ok(found);
        }
        if self.lookups >= MAX_CHANGED_INODES {
            return Ok(false);
        }
        let mut found = false;
        for &(_, state) in self.roots.iter().filter(|r| r.0 == tree) {
            let result = all_inside(&BTreeSet::from([inode]), state, |ino| {
                if self.lookups >= MAX_CHANGED_INODES {
                    return Err(WalkError::Capacity("state ownership lookups"));
                }
                self.lookups += 1;
                parents(source, tree, ino, boundary)
            });
            match result {
                Ok(true) => {
                    found = true;
                    break;
                }
                Err(WalkError::Stopped(reason)) => return Err(WalkError::Stopped(reason)),
                Ok(false) | Err(_) => {}
            }
        }
        if self.decisions.len() < MAX_CHANGED_INODES {
            self.decisions.insert((tree, inode), found);
        }
        Ok(found)
    }
}

/// The subvolumes holding a state directory of `own_state` (`(tree,
/// inode)`) whose generation moved from `before` to `after` only through
/// inodes inside it. Each subvolume is judged on its own. A failed
/// lookup leaves its subvolume out, so its change stays (fail closed);
/// only a stop request ends the search.
pub(super) fn state_dir_writes<S: MetadataSource + ?Sized>(
    source: &mut S,
    own_state: &BTreeSet<(u64, u64)>,
    before: &SubvolumeSet,
    after: &SubvolumeSet,
    boundary: &mut dyn BatchBoundary,
) -> Result<Vec<StateDirWrites>, WalkError> {
    let mut found = Vec::new();
    for &(tree, state) in own_state {
        let (Some(first), Some(last)) = (before.get(tree), after.get(tree)) else {
            continue;
        };
        if first.generation == last.generation {
            continue;
        }
        match only_state_writes(source, tree, state, first.generation, boundary) {
            Ok(Some(inodes)) => found.push(StateDirWrites {
                subvolume_id: tree,
                generation_before: first.generation,
                generation_after: last.generation,
                inodes,
            }),
            Err(WalkError::Stopped(reason)) => return Err(WalkError::Stopped(reason)),
            Ok(None) | Err(_) => {}
        }
    }
    Ok(found)
}

/// How many inodes of `tree` changed after transaction `since`, when
/// all of them lie inside directory `state`.
fn only_state_writes<S: MetadataSource + ?Sized>(
    source: &mut S,
    tree: u64,
    state: u64,
    since: u64,
    boundary: &mut dyn BatchBoundary,
) -> Result<Option<u64>, WalkError> {
    let changed = changed_inodes(source, tree, since, MAX_CHANGED_INODES, boundary)?;
    let inside = all_inside(&changed, state, |ino| parents(source, tree, ino, boundary))?;
    Ok(inside.then_some(changed.len() as u64))
}

/// Whether `reason` is the generation change `writes` explains.
pub(super) fn explained(reason: &ChangeReason, writes: &[StateDirWrites]) -> bool {
    let ChangeReason::SubvolumeGenerationChanged {
        subvolume_id,
        generation_before,
        generation_after,
        ..
    } = *reason
    else {
        return false;
    };
    writes.iter().any(|w| {
        (w.subvolume_id, w.generation_before, w.generation_after)
            == (subvolume_id, generation_before, generation_after)
    })
}

/// Whether every inode in `changed` is directory `state` or lies below
/// it, following each inode's `parents` up; every link of a hard-linked
/// inode must. Nothing changed is not "all inside": the change is then
/// unexplained. A failed lookup is the caller's to treat as not inside.
pub fn all_inside<E>(
    changed: &BTreeSet<u64>,
    state: u64,
    parents: impl FnMut(u64) -> Result<BTreeSet<u64>, E>,
) -> Result<bool, E> {
    let mut ancestry = Ancestry {
        state,
        parents,
        inside: HashSet::new(),
    };
    if changed.is_empty() {
        return Ok(false);
    }
    for &ino in changed {
        if !ancestry.inside(ino)? {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Ancestry<F> {
    state: u64,
    parents: F,
    /// Inodes already traced to the state directory.
    inside: HashSet<u64>,
}

impl<E, F: FnMut(u64) -> Result<BTreeSet<u64>, E>> Ancestry<F> {
    /// Follows every link of `start` up, iteratively. An inode reached
    /// twice, as in a cycle, counts as outside: that fails closed.
    fn inside(&mut self, start: u64) -> Result<bool, E> {
        let mut pending = vec![start];
        let mut seen = HashSet::new();
        while let Some(ino) = pending.pop() {
            if ino == self.state || self.inside.contains(&ino) {
                continue;
            }
            if !seen.insert(ino) || seen.len() > MAX_DEPTH {
                return Ok(false);
            }
            let parents = (self.parents)(ino)?;
            // The subvolume's root directory is its own parent.
            if parents.is_empty() || parents.contains(&ino) {
                return Ok(false);
            }
            pending.extend(parents);
        }
        self.inside.extend(seen);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation_changed(subvolume_id: u64, generation_after: u64) -> ChangeReason {
        ChangeReason::SubvolumeGenerationChanged {
            subvolume_id,
            selected: true,
            generation_before: 20,
            generation_after,
        }
    }

    #[test]
    fn state_ownership_requires_all_links_and_rejects_unlinked_or_cyclic_inodes() {
        use crate::btrfs::walk::testing::{FakeSource, NoStop};
        use crate::model::testing::*;
        let mut source = FakeSource::new(
            vec![
                inode_ref(301, 300, "ours"),
                inode_ref(302, 300, "inside"),
                inode_ref(302, 256, "outside"),
                inode_ref(304, 305, "cycle"),
                inode_ref(305, 304, "cycle"),
                inode_ref(306, 301, "nested"),
            ],
            3,
        );
        let mut state = StateOwnership {
            roots: [(256, 300)].into(),
            ..Default::default()
        };
        assert!(state.owns(&mut source, 256, 301, &mut NoStop).unwrap());
        assert!(state.owns(&mut source, 256, 306, &mut NoStop).unwrap());
        for inode in [302, 303, 304] {
            assert!(!state.owns(&mut source, 256, inode, &mut NoStop).unwrap());
        }
        assert!(!state.owns(&mut source, 257, 301, &mut NoStop).unwrap());
        let calls = source.calls.len();
        assert!(state.owns(&mut source, 256, 301, &mut NoStop).unwrap());
        assert_eq!(source.calls.len(), calls);
        state.lookups = MAX_CHANGED_INODES;
        assert!(!state.owns(&mut source, 256, 307, &mut NoStop).unwrap());
        assert_eq!(source.calls.len(), calls);
    }

    #[test]
    fn only_the_generation_change_the_writes_name_is_explained() {
        let writes = [StateDirWrites {
            subvolume_id: 256,
            generation_before: 20,
            generation_after: 21,
            inodes: 2,
        }];
        assert!(explained(&generation_changed(256, 21), &writes));
        assert!(!explained(&generation_changed(256, 22), &writes));
        assert!(!explained(&generation_changed(261, 21), &writes));
        let vanished = ChangeReason::SubvolumeVanished {
            subvolume_id: 256,
            generation_before: 20,
        };
        assert!(!explained(&vanished, &writes));
    }

    const STATE: u64 = 300;

    /// `/` 256 holds `/data` 257 with `/data/user` 258, and the state
    /// directory 300 with `cache` 301 holding 302; 303 is linked both in
    /// the state directory and in `/data`.
    fn parents(ino: u64) -> Result<BTreeSet<u64>, &'static str> {
        Ok(match ino {
            256 => BTreeSet::from([256]),
            257 | 300 => BTreeSet::from([256]),
            258 => BTreeSet::from([257]),
            301 => BTreeSet::from([300]),
            302 => BTreeSet::from([301]),
            303 => BTreeSet::from([300, 257]),
            _ => return Err("no such inode"),
        })
    }

    fn changed(inodes: &[u64]) -> BTreeSet<u64> {
        inodes.iter().copied().collect()
    }

    #[test]
    fn changes_only_in_the_state_directory_are_all_inside() {
        assert_eq!(
            all_inside(&changed(&[300, 301, 302]), STATE, parents),
            Ok(true)
        );
    }

    #[test]
    fn any_change_outside_the_state_directory_is_not() {
        assert_eq!(all_inside(&changed(&[301, 258]), STATE, parents), Ok(false));
        assert_eq!(all_inside(&changed(&[256]), STATE, parents), Ok(false));
    }

    #[test]
    fn a_hard_link_outside_the_state_directory_is_not_inside() {
        assert_eq!(all_inside(&changed(&[303]), STATE, parents), Ok(false));
    }

    #[test]
    fn a_failed_lookup_is_an_error() {
        assert_eq!(
            all_inside(&changed(&[301, 999]), STATE, parents),
            Err("no such inode")
        );
    }

    #[test]
    fn no_changed_inode_explains_nothing() {
        assert_eq!(all_inside(&changed(&[]), STATE, parents), Ok(false));
    }

    #[test]
    fn an_unlinked_inode_is_not_inside() {
        let unlinked = |_| Ok::<_, ()>(BTreeSet::new());
        assert_eq!(all_inside(&changed(&[400]), STATE, unlinked), Ok(false));
    }

    #[test]
    fn a_parent_cycle_is_not_inside() {
        let cycle = |ino| Ok::<_, ()>(BTreeSet::from([if ino == 400 { 401 } else { 400 }]));
        assert_eq!(all_inside(&changed(&[400]), STATE, cycle), Ok(false));
    }

    #[test]
    fn a_deeper_ancestry_than_the_limit_fails_closed() {
        let chain = |ino| Ok::<_, ()>(BTreeSet::from([ino - 1]));
        let deep = STATE + MAX_DEPTH as u64 + 1;
        assert_eq!(all_inside(&changed(&[deep]), STATE, chain), Ok(false));
        let within = STATE + MAX_DEPTH as u64;
        assert_eq!(all_inside(&changed(&[within]), STATE, chain), Ok(true));
    }

    #[test]
    fn each_directory_is_looked_up_once() {
        let mut lookups = Vec::new();
        let counting = |ino| {
            lookups.push(ino);
            parents(ino)
        };
        assert_eq!(all_inside(&changed(&[301, 302]), STATE, counting), Ok(true));
        assert_eq!(lookups, [301, 302]);
    }
}
