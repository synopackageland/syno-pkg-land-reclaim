//! The per-share answer of `snapshots` (SPEC 5.14): each snapshot's
//! exclusive and shared-with-live bytes, what only snapshots hold, and
//! what deleting a selected set frees.

use super::*;
use crate::output::JsonPath;
use crate::time::rfc3339;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where DSM deletes share snapshots.
pub const MANAGE_IN: &str = SNAPSHOT_REPLICATION;

/// One `--simulate-delete` selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Id(u64),
    Name(Vec<u8>),
    /// Taken longer than this before the answer.
    OlderThan(Duration),
    All,
}

/// Comma-separated selectors, as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub text: String,
    pub selectors: Vec<Selector>,
}

impl std::str::FromStr for Selection {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let selectors = text
            .split(',')
            .map(parse_selector)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Selection {
            text: text.to_string(),
            selectors,
        })
    }
}

fn parse_selector(text: &str) -> Result<Selector, String> {
    let (kind, value) = text.split_once(':').unwrap_or((text, ""));
    match kind {
        "all" if value.is_empty() => Ok(Selector::All),
        "id" => value
            .parse()
            .map(Selector::Id)
            .map_err(|_| format!("invalid subvolume id in {text:?}")),
        "name" if !value.is_empty() => Ok(Selector::Name(value.as_bytes().to_vec())),
        "older-than" => crate::resource::parse_duration(value).map(Selector::OlderThan),
        _ => Err(format!(
            "unknown selector {text:?}: use id:<subvolid>, name:<snapshot>, older-than:<duration> or all"
        )),
    }
}

impl Selection {
    /// Whether `snapshot` is selected at `now`.
    fn selects(&self, snapshot: &SnapshotInfo, now: SystemTime) -> bool {
        self.selectors.iter().any(|s| match s {
            Selector::All => true,
            Selector::Id(id) => snapshot.id == *id,
            Selector::Name(name) => snapshot.name == *name,
            Selector::OlderThan(age) => snapshot
                .otime
                .is_some_and(|t| UNIX_EPOCH + Duration::from_secs(t) + *age < now),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotJson {
    pub subvolume_id: u64,
    pub name: JsonPath,
    pub otime: Option<String>,
    pub read_only: bool,
    pub pending_deletion: bool,
    /// Its file extents were read; otherwise its values are `null`.
    pub materialized: bool,
    pub exclusive_bytes: Option<u64>,
    /// Unreferenced bytes inside extents freed by this snapshot; null if unmeasurable.
    pub pinned_unreferenced_bytes: Option<u64>,
    pub shared_with_live_bytes: Option<u64>,
    pub unknown_bytes: Option<u64>,
    pub qgroup_exclusive_bytes: Option<u64>,
}

/// Report-derived display figures, evaluated once at publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotFigures {
    pub snapshots: Vec<SnapshotJson>,
    pub held_bytes: u64,
    pub unknown_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SimulationJson {
    pub selector: String,
    pub selected_ids: Vec<u64>,
    pub freed_bytes: u64,
    pub unknown_bytes: u64,
    /// Selected snapshots whose file extents were not read: what they
    /// alone hold is not in `freed_bytes`.
    pub not_materialized_ids: Vec<u64>,
    /// The most the set may free; `null` when a selected snapshot was
    /// not read. `freed_bytes` is the least.
    pub upper_bound_bytes: Option<u64>,
    /// `freed_bytes` is what deleting the set frees.
    pub exact: bool,
    pub inexact_reasons: Vec<InexactReason>,
    /// The scan that removes what makes the answer inexact; `null` when
    /// it is exact or no scan option can make it so.
    pub exact_command: Option<String>,
}

/// Why a set answer is only bounded (SPEC 5.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InexactReason {
    NotMaterialized,
    UnknownOwners,
    CrossShareSharing,
}

impl InexactReason {
    /// The reasons behind `freed` with `not_materialized` snapshots unread.
    fn of(freed: &Freed, not_materialized: bool) -> Vec<InexactReason> {
        [
            (not_materialized, InexactReason::NotMaterialized),
            (freed.unknown_extents > 0, InexactReason::UnknownOwners),
            (
                freed.cross_share_extents > 0,
                InexactReason::CrossShareSharing,
            ),
        ]
        .into_iter()
        .filter_map(|(applies, reason)| applies.then_some(reason))
        .collect()
    }
}

/// How held files are shown: under which mount, and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listing<'a> {
    pub mount: &'a [u8],
    pub limit: usize,
}

/// Where a suggested rescan of a report's filesystem reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rescan<'a> {
    pub mount: &'a [u8],
    pub state_dir: &'a Path,
}

/// A file only snapshots hold (SPEC 5.14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldFileJson {
    pub snapshot_id: u64,
    pub snapshot: JsonPath,
    pub ino: u64,
    /// Below the snapshot root; `null` when the name is unresolved.
    pub path: Option<JsonPath>,
    /// The same path in the live share.
    pub live_path: Option<JsonPath>,
    /// The live share still has the file: rewritten, not deleted.
    pub in_live: bool,
    pub bytes: u64,
    pub unknown_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldFilesJson {
    /// Every file with snapshot-only data was tallied.
    pub complete: bool,
    /// Files tallied; `files` shows the largest of them.
    pub count: u64,
    pub files: Vec<HeldFileJson>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShareJson {
    pub share: JsonPath,
    pub live_subvolume_id: u64,
    pub held_only_by_snapshots_bytes: u64,
    pub unknown_bytes: u64,
    /// Snapshots whose file extents were not read (`--snapshots all`).
    pub snapshots_not_materialized: usize,
    pub snapshots: Vec<SnapshotJson>,
    /// Deleted snapshots awaiting the Btrfs cleaner.
    pub pending_deletion_ids: Vec<u64>,
    /// `null` for a report written before the list existed.
    pub held_files: Option<HeldFilesJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation: Option<SimulationJson>,
    pub manage_in: &'static str,
    /// Stable id of the DSM place (SPEC 4.6, GAP-14).
    pub manage_in_id: &'static str,
}

impl ShareJson {
    /// Whether some value is unknown: exit 3 (SPEC 5.12).
    pub fn has_unknown(&self) -> bool {
        self.unknown_bytes > 0
            || self.snapshots_not_materialized > 0
            || self
                .simulation
                .as_ref()
                .is_some_and(|s| s.unknown_bytes > 0 || !s.not_materialized_ids.is_empty())
    }

    pub fn exclusive_sum(&self) -> u64 {
        self.snapshots
            .iter()
            .filter_map(|s| s.exclusive_bytes)
            .sum()
    }
}

/// The answer for `share` of `analysis`.
pub fn share_answer(
    analysis: &SnapshotAnalysis,
    share: &ShareAnalysis,
    selection: Option<&Selection>,
    now: SystemTime,
    listing: &Listing<'_>,
    rescan: &Rescan<'_>,
) -> ShareJson {
    let figures = snapshot_figures(analysis, share);
    share_answer_from_figures(analysis, share, selection, now, listing, rescan, &figures)
}

pub fn snapshot_figures(analysis: &SnapshotAnalysis, share: &ShareAnalysis) -> SnapshotFigures {
    let snapshots = share
        .snapshots
        .iter()
        .map(|s| snapshot_json(analysis, share, s))
        .collect();
    let every: BTreeSet<u64> = share
        .snapshots
        .iter()
        .map(|s| s.id)
        .chain(share.pending.iter().copied())
        .collect();
    let held = analysis.freed(&every);
    SnapshotFigures {
        snapshots,
        held_bytes: held.bytes,
        unknown_bytes: held.unknown,
    }
}

/// Cached display figures may be absent in partially built derived state.
pub fn share_answer_cached(
    analysis: &SnapshotAnalysis,
    share: &ShareAnalysis,
    now: SystemTime,
    listing: &Listing<'_>,
    rescan: &Rescan<'_>,
    figures: &std::collections::BTreeMap<u64, SnapshotFigures>,
) -> ShareJson {
    match figures.get(&share.live) {
        Some(figures) => {
            share_answer_from_figures(analysis, share, None, now, listing, rescan, figures)
        }
        None => share_answer(analysis, share, None, now, listing, rescan),
    }
}

/// The same output and optional simulation using already computed display figures.
pub fn share_answer_from_figures(
    analysis: &SnapshotAnalysis,
    share: &ShareAnalysis,
    selection: Option<&Selection>,
    now: SystemTime,
    listing: &Listing<'_>,
    rescan: &Rescan<'_>,
    figures: &SnapshotFigures,
) -> ShareJson {
    ShareJson {
        share: JsonPath::new(&share.name),
        live_subvolume_id: share.live,
        held_only_by_snapshots_bytes: figures.held_bytes,
        unknown_bytes: figures.unknown_bytes,
        snapshots_not_materialized: share.skipped(),
        snapshots: figures.snapshots.clone(),
        pending_deletion_ids: share.pending.clone(),
        held_files: share
            .held_files
            .as_ref()
            .map(|held| held_files_json(share, held, listing)),
        simulation: selection.map(|sel| simulate(analysis, share, sel, now, rescan)),
        manage_in: manage_in_share(share),
        manage_in_id: manage_in_share_id(share),
    }
}

/// The first `listing.limit` held files of `share`, by live path.
pub fn held_files_json(
    share: &ShareAnalysis,
    held: &HeldFiles,
    listing: &Listing<'_>,
) -> HeldFilesJson {
    let root = live_root(listing.mount, share);
    HeldFilesJson {
        complete: held.complete,
        count: held.count,
        files: held
            .files
            .iter()
            .take(listing.limit)
            .map(|file| {
                let resolved = !file.path.is_empty();
                HeldFileJson {
                    snapshot_id: file.snapshot,
                    snapshot: JsonPath::new(
                        share
                            .snapshots
                            .iter()
                            .find(|s| s.id == file.snapshot)
                            .map_or(&[][..], |s| &s.name),
                    ),
                    ino: file.ino,
                    path: resolved.then(|| JsonPath::new(&file.path)),
                    live_path: resolved.then(|| JsonPath::new(&[&root[..], &file.path].concat())),
                    in_live: file.in_live,
                    bytes: file.bytes,
                    unknown_bytes: file.unknown_bytes,
                }
            })
            .collect(),
    }
}

/// The live share's root below `mount`, ending in `/`.
fn live_root(mount: &[u8], share: &ShareAnalysis) -> Vec<u8> {
    let below = match share.subvolume_path.is_empty() {
        true => &share.name,
        false => &share.subvolume_path,
    };
    let mut root = mount.to_vec();
    if !root.ends_with(b"/") {
        root.push(b'/');
    }
    root.extend_from_slice(below);
    root.push(b'/');
    root
}

fn snapshot_json(
    analysis: &SnapshotAnalysis,
    share: &ShareAnalysis,
    s: &SnapshotInfo,
) -> SnapshotJson {
    let alone = s
        .materialized
        .then(|| analysis.freed(&BTreeSet::from([s.id])));
    SnapshotJson {
        subvolume_id: s.id,
        name: JsonPath::new(&s.name),
        otime: s
            .otime
            .map(|t| rfc3339(UNIX_EPOCH + Duration::from_secs(t))),
        read_only: true,
        pending_deletion: false,
        materialized: s.materialized,
        exclusive_bytes: alone.map(|f| f.bytes),
        pinned_unreferenced_bytes: s
            .materialized
            .then(|| analysis.pinned_unreferenced(s.id))
            .flatten(),
        shared_with_live_bytes: s
            .materialized
            .then(|| analysis.shared_between(s.id, share.live)),
        unknown_bytes: alone.map(|f| f.unknown),
        qgroup_exclusive_bytes: analysis.qgroup_exclusive(s.id),
    }
}

fn simulate(
    analysis: &SnapshotAnalysis,
    share: &ShareAnalysis,
    selection: &Selection,
    now: SystemTime,
    rescan: &Rescan<'_>,
) -> SimulationJson {
    let ids = share
        .snapshots
        .iter()
        .filter(|s| selection.selects(s, now))
        .map(|s| s.id)
        .collect();
    simulate_set(analysis, selection.text.clone(), &ids, rescan)
}

/// Deleting the snapshots `ids` together, with the pending deletions of
/// their shares.
pub fn simulate_set(
    analysis: &SnapshotAnalysis,
    selector: String,
    ids: &BTreeSet<u64>,
    rescan: &Rescan<'_>,
) -> SimulationJson {
    let selected: Vec<(&ShareAnalysis, &SnapshotInfo)> =
        ids.iter().filter_map(|&id| analysis.snapshot(id)).collect();
    let freed = if selected.is_empty() {
        Freed::default()
    } else {
        analysis.freed(
            &ids.iter()
                .copied()
                .chain(
                    selected
                        .iter()
                        .flat_map(|(share, _)| share.pending.iter().copied()),
                )
                .collect(),
        )
    };
    let not_materialized_ids: Vec<u64> = selected
        .iter()
        .filter(|(_, s)| !s.materialized)
        .map(|(_, s)| s.id)
        .collect();
    let inexact_reasons = InexactReason::of(&freed, !not_materialized_ids.is_empty());
    let exact_command = exact_command(analysis, &selected, &inexact_reasons, rescan);
    SimulationJson {
        selector,
        selected_ids: ids.iter().copied().collect(),
        freed_bytes: freed.bytes,
        unknown_bytes: freed.unknown,
        upper_bound_bytes: not_materialized_ids
            .is_empty()
            .then(|| freed.bytes + freed.unknown + freed.cross_share),
        not_materialized_ids,
        exact: inexact_reasons.is_empty(),
        inexact_reasons,
        exact_command,
    }
}

/// Budgets a suggested rescan multiplies the report's budgets by.
const RESCAN_BUDGET_FACTOR: u64 = 4;

/// The scan of the selection's share, or of the volume for a set across
/// shares, that reads what `reasons` say is missing.
fn exact_command(
    analysis: &SnapshotAnalysis,
    selected: &[(&ShareAnalysis, &SnapshotInfo)],
    reasons: &[InexactReason],
    rescan: &Rescan<'_>,
) -> Option<String> {
    let unmeasured = reasons.iter().any(|r| {
        matches!(
            r,
            InexactReason::NotMaterialized | InexactReason::UnknownOwners
        )
    });
    let mut flags = Vec::new();
    if unmeasured && selected.iter().any(|(share, _)| share.skipped() > 0) {
        flags.push("--snapshots all".to_string());
    }
    if reasons.contains(&InexactReason::UnknownOwners)
        && analysis.stats.owner_unresolved_extents > 0
    {
        flags.push(format!(
            "--owner-budget {} --owner-budget-global {}",
            u64::from(analysis.owner_budget).saturating_mul(RESCAN_BUDGET_FACTOR),
            analysis
                .global_owner_budget
                .saturating_mul(RESCAN_BUDGET_FACTOR)
        ));
    }
    if flags.is_empty() {
        return None;
    }
    let shares: BTreeSet<u64> = selected.iter().map(|(share, _)| share.live).collect();
    let target = match selected {
        [(share, _), ..] if shares.len() == 1 => {
            let mut root = live_root(rescan.mount, share);
            root.pop();
            root
        }
        _ => rescan.mount.to_vec(),
    };
    Some(
        crate::document::ScanHint::new(&target, rescan.state_dir, None)
            .with_flags(&flags.join(" "))
            .command,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE: u64 = 256;
    const DAY: u64 = 86_400;
    /// The answer's clock: day 100.
    const NOW: u64 = 100 * DAY;

    fn info(id: u64, day: u64, materialized: bool) -> SnapshotInfo {
        SnapshotInfo {
            id,
            name: format!("GMT+08-day{day}").into_bytes(),
            subvolume_path: Vec::new(),
            otime: Some(day * DAY),
            generation: 1,
            materialized,
        }
    }

    fn holding(owners: &[u64], hold: Hold, bytes: u64) -> Holding {
        Holding {
            owners: owners.to_vec(),
            hold,
            bytes,
            extents: 1,
            pinned_unreferenced_bytes: (hold == Hold::Resolved && owners.len() == 1).then_some(0),
        }
    }

    fn held(snapshot: u64, ino: u64, path: &[u8], in_live: bool, bytes: u64) -> HeldFile {
        HeldFile {
            snapshot,
            ino,
            path: path.to_vec(),
            in_live,
            bytes,
            unknown_bytes: 0,
        }
    }

    /// Snapshots 303 (day 90), 302 (day 50), 301 (day 5, not read).
    fn analysis() -> SnapshotAnalysis {
        SnapshotAnalysis {
            depth: Depth::Newest(2),
            owner_budget: DEFAULT_OWNER_BUDGET,
            global_owner_budget: DEFAULT_GLOBAL_OWNER_BUDGET,
            shares: vec![ShareAnalysis {
                live: LIVE,
                name: b"backup".to_vec(),
                subvolume_path: b"backup".to_vec(),
                snapshots: vec![
                    info(303, 90, true),
                    info(302, 50, true),
                    info(301, 5, false),
                ],
                pending: vec![399],
                held_files: Some(HeldFiles {
                    complete: false,
                    count: 3,
                    files: vec![
                        held(303, 258, b"dir/kept", true, 40),
                        held(302, 257, b"gone", false, 10),
                        held(302, 260, b"", false, 0),
                    ],
                }),
            }],
            holdings: vec![
                holding(&[302], Hold::Resolved, 10),
                holding(&[303], Hold::Resolved, 20),
                holding(&[302, 303], Hold::Resolved, 40),
                holding(&[LIVE, 302, 303], Hold::Resolved, 80),
                holding(&[302], Hold::Unknown, 160),
            ],
            quota: Some(QuotaCheck {
                consistent: true,
                exclusive: BTreeMap::from([(302, 11)]),
            }),
            stats: AnalysisStats::default(),
        }
    }

    fn answer(selection: Option<&str>) -> ShareJson {
        let a = analysis();
        let selection = selection.map(|s| s.parse::<Selection>().unwrap());
        share_answer(
            &a,
            &a.shares[0],
            selection.as_ref(),
            UNIX_EPOCH + Duration::from_secs(NOW),
            &listing(),
            &rescan(),
        )
    }

    fn listing() -> Listing<'static> {
        Listing {
            mount: b"/volume1",
            limit: 2,
        }
    }

    fn rescan() -> Rescan<'static> {
        Rescan {
            mount: b"/volume1",
            state_dir: Path::new("/s"),
        }
    }

    #[test]
    fn absent_cached_share_falls_back_to_original_snapshot_analysis() {
        let original = analysis();
        let now = UNIX_EPOCH + Duration::from_secs(NOW);
        let figures = std::collections::BTreeMap::new();
        assert_eq!(
            share_answer_cached(
                &original,
                &original.shares[0],
                now,
                &listing(),
                &rescan(),
                &figures
            ),
            share_answer(
                &original,
                &original.shares[0],
                None,
                now,
                &listing(),
                &rescan()
            )
        );
    }

    #[test]
    fn cached_figures_keep_unknowns_qgroups_held_file_limits_and_lossless_names() {
        let mut original = analysis();
        original.shares[0].name = b"backup-\xff".to_vec();
        original.shares[0].subvolume_path = b"@iSCSI/lun".to_vec();
        let figures = snapshot_figures(&original, &original.shares[0]);
        let encoded = serde_json::to_vec(&figures).unwrap();
        let figures: SnapshotFigures = serde_json::from_slice(&encoded).unwrap();
        let mut compact = original.clone();
        compact.holdings.clear();
        compact.quota = None;
        for limit in [0, 2, 100] {
            let listing = Listing { limit, ..listing() };
            let now = UNIX_EPOCH + Duration::from_secs(NOW);
            assert_eq!(
                share_answer(
                    &original,
                    &original.shares[0],
                    None,
                    now,
                    &listing,
                    &rescan()
                ),
                share_answer_from_figures(
                    &compact,
                    &compact.shares[0],
                    None,
                    now,
                    &listing,
                    &rescan(),
                    &figures
                )
            );
        }
    }

    #[test]
    fn the_largest_held_files_show_where_they_were_in_the_live_share() {
        let files = answer(None).held_files.unwrap();
        assert!(!files.complete);
        assert_eq!(files.count, 3);
        assert_eq!(files.files.len(), 2, "--limit shows the first files only");
        let kept = &files.files[0];
        assert_eq!(kept.snapshot_id, 303);
        assert_eq!(kept.snapshot, JsonPath::new(b"GMT+08-day90"));
        assert_eq!(kept.path, Some(JsonPath::new(b"dir/kept")));
        assert_eq!(
            kept.live_path,
            Some(JsonPath::new(b"/volume1/backup/dir/kept"))
        );
        assert!(kept.in_live);
        assert_eq!((kept.bytes, kept.unknown_bytes), (40, 0));
        assert_eq!(files.files[1].snapshot, JsonPath::new(b"GMT+08-day50"));
    }

    #[test]
    fn an_unresolved_name_has_no_path_and_old_reports_have_no_list() {
        let a = analysis();
        let wide = Listing {
            limit: 10,
            ..listing()
        };
        let answer = share_answer(&a, &a.shares[0], None, UNIX_EPOCH, &wide, &rescan());
        let lost = &answer.held_files.unwrap().files[2];
        assert_eq!((lost.ino, &lost.path, &lost.live_path), (260, &None, &None));

        let mut old = a.shares[0].clone();
        old.held_files = None;
        assert_eq!(
            share_answer(&a, &old, None, UNIX_EPOCH, &wide, &rescan()).held_files,
            None
        );
    }

    #[test]
    fn each_snapshot_shows_what_it_alone_frees_and_what_it_shares_with_live() {
        let share = answer(None);
        let row = |id| {
            share
                .snapshots
                .iter()
                .find(|s| s.subvolume_id == id)
                .unwrap()
                .clone()
        };
        let old = row(302);
        assert_eq!(old.exclusive_bytes, Some(10));
        assert_eq!(old.pinned_unreferenced_bytes, None);
        assert_eq!(old.unknown_bytes, Some(160));
        assert_eq!(old.shared_with_live_bytes, Some(80));
        assert_eq!(old.qgroup_exclusive_bytes, Some(11));
        assert_eq!(old.otime.as_deref(), Some("1970-02-20T00:00:00Z"));
        assert_eq!(row(303).exclusive_bytes, Some(20));
        assert_eq!(row(303).pinned_unreferenced_bytes, Some(0));
        assert_eq!(row(303).qgroup_exclusive_bytes, None);
        let skipped = row(301);
        assert!(!skipped.materialized);
        assert_eq!(skipped.exclusive_bytes, None);
        assert_eq!(skipped.pinned_unreferenced_bytes, None);
        assert_eq!(skipped.shared_with_live_bytes, None);
    }

    #[test]
    fn lun_management_hint_uses_the_path_captured_in_the_report() {
        let a = analysis();
        let mut share = a.shares[0].clone();
        share.subvolume_path = b"backup/lun-uuid".to_vec();
        share.snapshots[0].subvolume_path = b"@syno/@iSCSI/Snapshot/lun-uuid".to_vec();

        let answer = share_answer(&a, &share, None, UNIX_EPOCH, &listing(), &rescan());

        assert_eq!(answer.manage_in, SAN_MANAGER);
    }

    #[test]
    fn what_only_snapshots_hold_is_more_than_the_sum_of_exclusives() {
        let share = answer(None);
        assert_eq!(share.held_only_by_snapshots_bytes, 70);
        assert_eq!(share.exclusive_sum(), 30);
        assert_eq!(share.unknown_bytes, 160);
        assert_eq!(share.snapshots_not_materialized, 1);
        assert_eq!(share.pending_deletion_ids, vec![399]);
        assert!(share.has_unknown());
    }

    #[test]
    fn a_selection_frees_one_set_operation_over_its_members() {
        let sim = answer(Some("older-than:30d")).simulation.unwrap();
        assert_eq!(sim.selected_ids, vec![301, 302]);
        assert_eq!(sim.freed_bytes, 10);
        assert_eq!(sim.unknown_bytes, 160);
        assert_eq!(sim.not_materialized_ids, vec![301]);
        let both = answer(Some("id:302,name:GMT+08-day90")).simulation.unwrap();
        assert_eq!(both.selected_ids, vec![302, 303]);
        assert_eq!(both.freed_bytes, 70);
        assert!(both.not_materialized_ids.is_empty());
        let none = answer(Some("id:1")).simulation.unwrap();
        assert_eq!((none.freed_bytes, none.unknown_bytes), (0, 0));
        assert_eq!(
            answer(Some("all")).simulation.unwrap().selected_ids.len(),
            3
        );
    }

    #[test]
    fn a_selection_says_whether_its_freed_bytes_are_exact() {
        let alone = answer(Some("id:303")).simulation.unwrap();
        assert_eq!(alone.upper_bound_bytes, Some(20));
        assert!(alone.exact);
        assert!(alone.inexact_reasons.is_empty());
        let both = answer(Some("id:302,id:303")).simulation.unwrap();
        assert_eq!((both.freed_bytes, both.upper_bound_bytes), (70, Some(230)));
        assert!(!both.exact);
        assert_eq!(both.inexact_reasons, vec![InexactReason::UnknownOwners]);
        let unread = answer(Some("older-than:30d")).simulation.unwrap();
        assert_eq!(unread.upper_bound_bytes, None);
        assert_eq!(
            unread.inexact_reasons,
            vec![InexactReason::NotMaterialized, InexactReason::UnknownOwners]
        );
    }

    #[test]
    fn a_set_across_shares_bounds_what_their_reflinks_may_free() {
        let mut a = analysis();
        let mut other = a.shares[0].clone();
        other.live = 500;
        other.snapshots = vec![info(501, 90, true)];
        other.pending.clear();
        a.shares.push(other);
        a.holdings.push(holding(&[303], Hold::External, 5));
        a.holdings.push(holding(&[501], Hold::External, 5));

        let within = simulate_set(&a, String::new(), &BTreeSet::from([303]), &rescan());
        assert!(within.exact, "{within:?}");
        let across = simulate_set(&a, String::new(), &BTreeSet::from([303, 501]), &rescan());
        assert_eq!(
            (across.freed_bytes, across.upper_bound_bytes),
            (20, Some(30))
        );
        assert_eq!(
            across.inexact_reasons,
            vec![InexactReason::CrossShareSharing]
        );
    }

    #[test]
    fn an_inexact_selection_names_the_rescan_that_may_make_it_exact() {
        let exact = answer(Some("id:303")).simulation.unwrap();
        assert_eq!(exact.exact_command, None);
        let unread = answer(Some("older-than:30d")).simulation.unwrap();
        assert_eq!(
            unread.exact_command.as_deref(),
            Some("synoreclaim scan /volume1/backup --state-dir /s --snapshots all")
        );
        let mut a = analysis();
        a.stats.owner_unresolved_extents = 2;
        let over_budget = simulate_set(&a, String::new(), &BTreeSet::from([302]), &rescan());
        assert_eq!(
            over_budget.exact_command.as_deref(),
            Some(
                "synoreclaim scan /volume1/backup --state-dir /s --snapshots all --owner-budget 256 --owner-budget-global 400000"
            )
        );
    }

    #[test]
    fn no_rescan_makes_sharing_across_shares_exact_and_a_set_across_shares_rescans_the_volume() {
        let mut a = analysis();
        let mut other = a.shares[0].clone();
        other.live = 500;
        other.name = b"photos".to_vec();
        other.subvolume_path = b"photos".to_vec();
        other.snapshots = vec![info(501, 90, true)];
        a.shares.push(other);
        a.holdings.push(holding(&[303], Hold::External, 5));
        let across = simulate_set(&a, String::new(), &BTreeSet::from([303, 501]), &rescan());
        assert_eq!(across.exact_command, None);
        let unread = simulate_set(&a, String::new(), &BTreeSet::from([301, 501]), &rescan());
        assert_eq!(
            unread.exact_command.as_deref(),
            Some("synoreclaim scan /volume1 --state-dir /s --snapshots all")
        );
    }

    #[test]
    fn selectors_parse_or_say_what_is_accepted() {
        let s: Selection = "id:7,name:a:b,older-than:2d,all".parse().unwrap();
        assert_eq!(
            s.selectors,
            vec![
                Selector::Id(7),
                Selector::Name(b"a:b".to_vec()),
                Selector::OlderThan(Duration::from_secs(2 * DAY)),
                Selector::All,
            ]
        );
        for bad in ["id:x", "older-than:5", "name:", "latest", "all:1"] {
            assert!(bad.parse::<Selection>().is_err(), "{bad}");
        }
    }
}
