//! `cleanup plan`, `apply` and `delete` (SPEC 5.7): every selected item in
//! a shared folder moves into that shared folder's DSM Recycle Bin with
//! DSM's own tool; nothing is removed for good. A plan is a short list of
//! checked paths in root's private state; apply checks each again, moves
//! it and checks with `lstat` what moved.

use super::*;
use crate::recycle_move::{self as moves, BinStatus, Kind, Outcome, Plan, Size, Target, Tools};
use serde::Deserialize;
use std::collections::HashMap;
use std::ffi::OsString;

/// A selected item as the plan and the review show it.
#[derive(Debug, Clone, Serialize)]
struct PlanItem {
    path: JsonPath,
    kind: Kind,
    #[serde(flatten)]
    size: Size,
    /// Symlinks it holds, itself included: a Recycle Bin drops them.
    #[serde(skip_serializing_if = "is_zero")]
    symlinks: u64,
    /// In a user's Synology Drive folder: its clients sync the move.
    drive: bool,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// One shared folder's part of a plan.
#[derive(Debug, Clone, Serialize)]
struct PlanShare {
    share: JsonPath,
    path: JsonPath,
    recycle_bin: BinStatus,
    /// Selected items in this shared folder.
    item_count: u64,
    /// How many of them are in a user's Synology Drive folder.
    drive_items: u64,
    /// The first [`PREVIEW_ITEMS`] of them.
    items: Vec<PlanItem>,
    #[serde(flatten)]
    size: Size,
    /// Symlinks its items hold: DSM's Recycle Bin drops them on a move,
    /// as File Station's delete does, and a restore does not bring them back.
    #[serde(skip_serializing_if = "is_zero")]
    symlinks: u64,
    /// No Recycle Bin takes its items, so apply removes them for good once
    /// the user confirms that; `delete_reason` says why.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    delete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_reason: Option<DeleteReason>,
}

/// Why a plan removes a folder's items for good (user decisions
/// 2026-11-14): there is no Recycle Bin to move them to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DeleteReason {
    /// A folder right below the volume that DSM does not list as a
    /// shared folder.
    NoSharedFolder,
    /// A shared folder whose Recycle Bin is off.
    RecycleBinOff,
}

/// Items a plan lists per shared folder, and paths it lists in all: the
/// review shows totals and a preview, never a list of every item.
const PREVIEW_ITEMS: usize = 100;

#[derive(Debug, Serialize)]
struct PlanDocument {
    #[serde(flatten)]
    envelope: Envelope,
    plan_id: String,
    plan_file: JsonPath,
    expires_at: String,
    /// How many items the plan moves or removes.
    items: u64,
    /// How many of them it removes for good (no Recycle Bin: no shared
    /// folder, or its Recycle Bin is off): apply needs `--confirm-delete`.
    deletes: u64,
    /// The first [`PREVIEW_ITEMS`] of them, readable.
    paths: Vec<JsonPath>,
    /// What the plan moves, as allocated now.
    size: Size,
    /// Every selected item by shared folder, those that cannot move too.
    shares: Vec<PlanShare>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duplicates: Option<crate::duplicates::command::PlanSummary>,
}

/// What a plan, a refusal or a delete is about: the first selected path.
fn subject(targets: &TargetArgs) -> Vec<u8> {
    first_operand(targets)
}

/// Checked items with their resolution, and the refused paths.
type Checked = (Vec<(Resolved, Target)>, Vec<TargetError>);

/// Why a folder right below the volume named `name` cannot be cleaned up
/// when DSM does not list it among `shares`: it is no shared folder, so it
/// lies outside every one and has no Recycle Bin. `None` for a DSM share,
/// which stays a `share_root`.
fn outside_every_share(
    name: &[u8],
    shares: &std::collections::HashSet<Vec<u8>>,
) -> Option<&'static str> {
    (!shares.contains(name)).then_some(
        "protected cleanup target: a folder right below the volume that is not a DSM shared folder, so there is no Recycle Bin to move it into",
    )
}

/// Why items in folder `name` right below the volume, which is no shared
/// folder, are never removed for good: a hidden or file-system folder
/// (`.…`, `lost+found`). `@…` and `#…` folders never get this far
/// ([`moves::locate`]).
fn hidden_system_folder(name: &[u8]) -> Option<&'static str> {
    (name.starts_with(b".") || name == b"lost+found").then_some(
        "protected cleanup target: a hidden or file-system folder right below the volume",
    )
}

/// Each selected path checked and placed in its shared folder; refused
/// paths go to `errors`.
fn check_targets(env: &Environment, targets: &TargetArgs) -> Result<Checked, Failure> {
    let (resolved, mut errors) = resolve_targets(env, targets)?;
    let mut checked: Vec<(Resolved, Target)> = Vec::new();
    // DSM's shared folders of each volume, read once per plan.
    let mut inventories: HashMap<PathBuf, Result<std::collections::HashSet<Vec<u8>>, String>> =
        HashMap::new();
    let total = resolved.len() as u64;
    crate::planning::step(crate::planning::Step::Items);
    for (done, target) in resolved.into_iter().enumerate() {
        crate::planning::progress(done as u64, total);
        if done % 256 == 0 && crate::sys::stop_requested() {
            return Err(Refusal::new("cancelled", "stopped while checking the selection").into());
        }
        let path = target.path.as_os_str().as_bytes().to_vec();
        let mount = target.mount.as_os_str().as_bytes();
        let refuse = |code: &'static str, message: String| {
            TargetError::new(JsonPath::new(&path), code, message)
        };
        let protect = |reason: &'static str, package: Option<&'static str>, message: String| {
            refuse("protected_target", message).protected(reason, package)
        };
        if target.scope != Scope::Path {
            errors.push(protect(
                "volume",
                None,
                "protected cleanup target: a volume; select items inside a shared folder".into(),
            ));
            continue;
        }
        let located = match moves::locate(&path, mount) {
            Ok(located) => located,
            Err(refusal) => {
                let mut error = refuse(refusal.code, refusal.message);
                if let Some(reason) = refusal.reason {
                    error = error.protected(reason, None);
                }
                let name = path.rsplit(|b| *b == b'/').next().unwrap_or_default();
                if refusal.reason == Some("share_root")
                    && let Ok(shares) =
                        inventories.entry(target.mount.clone()).or_insert_with(|| {
                            crate::dsm_shares::inventory(&target.mount)
                                .map(|shares| shares.into_iter().collect())
                                .map_err(|e| e.to_string())
                        })
                    && let Some(message) = outside_every_share(name, shares)
                {
                    error = error.protected("outside_share", None);
                    error.message = message.into();
                }
                errors.push(error);
                continue;
            }
        };
        match guard::target_refusal(&target.path, &target.mount) {
            Ok(None) => {}
            Ok(Some(why)) => {
                let (reason, package) = guard::protected_reason(&target.path, &target.mount);
                errors.push(protect(
                    reason,
                    package,
                    format!("protected cleanup target: {why}"),
                ));
                continue;
            }
            Err(e) => {
                errors.push(refuse(guard::failure_code(&e), e.to_string()));
                continue;
            }
        }
        let file = match guard::open_absolute_no_follow(&target.path) {
            Ok(file) => file,
            Err(e) => {
                errors.push(refuse(guard::failure_code(&e), e.to_string()));
                continue;
            }
        };
        let meta = file.metadata()?;
        if target.filesystem == crate::filesystem::FilesystemKind::Btrfs
            && guard::is_subvolume_boundary(meta.mode() & libc::S_IFMT, meta.ino())
        {
            errors.push(refuse(
                "selection_boundary",
                "cleanup selection is a Btrfs subvolume; manage it in DSM".into(),
            ));
            continue;
        }
        let shares = match inventories.entry(target.mount.clone()).or_insert_with(|| {
            crate::dsm_shares::inventory(&target.mount)
                .map(|shares| shares.into_iter().collect())
                .map_err(|e| e.to_string())
        }) {
            Ok(shares) => shares,
            Err(e) => {
                errors.push(refuse(
                    "recycle_bin_unavailable",
                    format!("the shared folders of this volume could not be read: {e}"),
                ));
                continue;
            }
        };
        // In a folder right below the volume that DSM does not list as a
        // shared folder: no Recycle Bin, so the plan removes it for good,
        // and apply only once the user confirmed that (user decision
        // 2026-11-14). That folder itself stays refused (`outside_share`).
        let delete = !shares.contains(&located.share);
        if delete && let Some(why) = hidden_system_folder(&located.share) {
            errors.push(protect("system", None, why.into()));
            continue;
        }
        checked.push((
            target.clone(),
            Target {
                path: path.clone(),
                share: located.share,
                rpath: located.rpath,
                kind: if meta.is_dir() {
                    Kind::Directory
                } else {
                    Kind::File
                },
                device: meta.dev(),
                inode: meta.ino(),
                tree: (target.filesystem == crate::filesystem::FilesystemKind::Btrfs)
                    .then_some(target.tree),
                size: Size::default(),
                symlinks: 0,
                split: Vec::new(),
                delete,
            },
        ));
    }
    crate::planning::progress(total, total);
    Ok((checked, errors))
}

/// Each item's size, and the folders below it the move splits, from the
/// newest report covering it whose entry is still the item (same inode):
/// one streaming read per report, no walk of the items. An item no report
/// covers is walked instead.
fn size_targets(state_dir: &Path, checked: &mut [(Resolved, Target)]) -> Result<(), Failure> {
    let store = Store::open(state_dir, sys::effective_uid())?;
    let mut by_report: Vec<(PathBuf, Vec<usize>)> = Vec::new();
    let mut uncovered = Vec::new();
    // The reports of each filesystem, listed once.
    let mut listings = HashMap::new();
    for (index, (resolved, _)) in checked.iter().enumerate() {
        let fsid = crate::resolve::fsid_text(&resolved.fsid);
        let listing = match listings.entry(fsid) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let reports = store.display_reports(e.key())?.reports;
                e.insert(reports)
            }
        };
        let path = resolved.path.as_os_str().as_bytes();
        match listing.iter().find(|(_, h)| query::covers(h, path)) {
            Some((report, _)) => match by_report.iter_mut().find(|(r, _)| r == report) {
                Some((_, indices)) => indices.push(index),
                None => by_report.push((report.clone(), vec![index])),
            },
            None => uncovered.push(index),
        }
    }
    for (report, indices) in by_report {
        let (_, reader) = open_report(&report)?;
        let mut sizing = Sizing::new(checked, &indices);
        reader.for_each(|path, record| {
            sizing.visit(
                path,
                record.depth,
                record.kind == crate::model::namespace::FileKind::Directory,
                record.kind == crate::model::namespace::FileKind::Symlink,
                record.objectid,
                Size {
                    entries: record.metrics.entries,
                    files: record.metrics.files,
                    allocated_bytes: record.metrics.physical,
                },
            );
            std::ops::ControlFlow::Continue(())
        })?;
        let sized = sizing.sized;
        uncovered.extend(
            indices
                .iter()
                .zip(&sized)
                .filter(|(_, s)| !**s)
                .map(|(i, _)| *i),
        );
    }
    for index in uncovered {
        let target = &mut checked[index].1;
        let walked = moves::measure(&moves::os_path(&target.path), 0, u64::MAX)?;
        if !walked.complete {
            return Err(Refusal::new("cancelled", "stopped while measuring the selection").into());
        }
        target.size = walked.size;
        target.symlinks = walked.symlinks;
    }
    Ok(())
}

/// One report read for the selected items it covers: each item's size
/// when the report's entry is still the item, and the folders below it
/// holding more than [`moves::SPLIT_ENTRIES`] entries. Records come in
/// preorder, so the items open around a record are a stack.
struct Sizing<'a> {
    checked: &'a mut [(Resolved, Target)],
    indices: &'a [usize],
    /// Each item's path to its slot in `indices`.
    slots: HashMap<Vec<u8>, usize>,
    sized: Vec<bool>,
    open: Vec<(usize, u32)>,
}

impl<'a> Sizing<'a> {
    fn new(checked: &'a mut [(Resolved, Target)], indices: &'a [usize]) -> Self {
        let slots = indices
            .iter()
            .enumerate()
            .map(|(slot, &i)| (checked[i].1.path.clone(), slot))
            .collect();
        Sizing {
            checked,
            indices,
            slots,
            sized: vec![false; indices.len()],
            open: Vec::new(),
        }
    }

    fn visit(
        &mut self,
        path: &[u8],
        depth: u32,
        dir: bool,
        symlink: bool,
        objectid: u64,
        size: Size,
    ) {
        while self.open.last().is_some_and(|(_, open)| depth <= *open) {
            self.open.pop();
        }
        // A symlink counts for every open item it lies in: the report has
        // a record for each entry, so this needs no walk of its own.
        if symlink {
            for &(slot, _) in &self.open {
                self.checked[self.indices[slot]].1.symlinks += 1;
            }
        }
        if let Some(&slot) = self.slots.get(path) {
            let target = &mut self.checked[self.indices[slot]].1;
            if objectid == target.inode {
                target.size = size;
                target.symlinks = u64::from(symlink);
                self.sized[slot] = true;
                if dir {
                    self.open.push((slot, depth));
                }
            }
        }
        if dir && size.entries > moves::SPLIT_ENTRIES {
            for &(slot, _) in &self.open {
                let target = &mut self.checked[self.indices[slot]].1;
                if let Some(rest) = path.strip_prefix(target.path.as_slice()) {
                    target
                        .split
                        .push(rest.strip_prefix(b"/").unwrap_or(rest).to_vec());
                }
            }
        }
    }
}

/// The configuration's `allowed_roots`, when set, only narrows what
/// cleanup may touch (SPEC 5.7 `config`).
/// Names the duplicate group each refused path is a copy of, or is inside
/// of: taking that group out of the selection drops the path.
fn name_groups(errors: &mut [TargetError], group_of: &HashMap<Vec<u8>, String>) {
    if group_of.is_empty() {
        return;
    }
    for error in errors {
        let path = error.target.bytes();
        let mut end = path.len();
        let group = loop {
            if let Some(group) = group_of.get(&path[..end]) {
                break Some(group);
            }
            match path[..end].iter().rposition(|b| *b == b'/') {
                Some(slash) if slash > 0 => end = slash,
                _ => break None,
            }
        };
        if let Some(group) = group {
            error.duplicate.get_or_insert_default().group = Some(group.clone());
        }
    }
}

fn within_allowed_roots(config: &guard::CleanupConfig, path: &[u8]) -> Result<(), Refusal> {
    let Some(roots) = &config.allowed_roots else {
        return Ok(());
    };
    let path = moves::os_path(path);
    if roots
        .iter()
        .any(|root| Path::new(root).is_absolute() && path.starts_with(root))
    {
        Ok(())
    } else {
        Err(Refusal::new(
            "outside_allowed_roots",
            "cleanup target is outside the configured allowed roots",
        ))
    }
}

/// An `expect` entry of `cleanup.plan`: the report's inode of the path.
#[derive(Debug, Clone, Deserialize)]
pub struct ExpectedPath {
    #[serde(with = "crate::report::hex_bytes")]
    pub path: Vec<u8>,
    pub expect: ExpectedIdentity,
}

/// What a report row identified (`expect`); only the inode is compared.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExpectedIdentity {
    pub objectid: u64,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

#[allow(clippy::too_many_arguments)]
pub fn cleanup_plan(
    selection: (TargetArgs, Option<String>, Option<String>, Option<String>),
    output_path: Option<PathBuf>,
    json: bool,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let (targets, expect, duplicates, staged) = selection;
    let subject = subject(&targets);
    let result = make_plan(
        Selected {
            targets,
            expect,
            duplicates,
            staged,
        },
        output_path,
        state_dir,
        &Tools::dsm(),
    );
    match result {
        Ok(document) => {
            emit(json, &document, || render_plan(&document))?;
            Ok(0)
        }
        Err(error) => finish_cleanup(json, "cleanup-plan", &subject, Err(error)),
    }
}

/// What `cleanup plan` was asked to plan: paths, their report identities,
/// duplicate groups, and a selection `cleanup select` staged.
struct Selected {
    targets: TargetArgs,
    expect: Option<String>,
    duplicates: Option<String>,
    staged: Option<String>,
}

fn make_plan(
    selected: Selected,
    output_path: Option<PathBuf>,
    state_dir: &Path,
    tools: &Tools,
) -> Result<PlanDocument, Failure> {
    let Selected {
        mut targets,
        expect,
        mut duplicates,
        staged,
    } = selected;
    let mut expected: Vec<ExpectedPath> = match expect.as_deref() {
        Some(text) => serde_json::from_str(text)
            .map_err(|e| Refusal::new("invalid_expect", format!("invalid expect: {e}")))?,
        None => Vec::new(),
    };
    if let Some(id) = staged.as_deref() {
        let staged = take_selection(state_dir, id)?;
        targets
            .operands
            .extend(staged.paths.iter().map(|p| OsString::from_vec(p.clone())));
        expected.extend(staged.expected);
        if !staged.groups.is_empty() {
            let mut spec: serde_json::Value =
                serde_json::from_str(duplicates.as_deref().ok_or_else(|| {
                    Refusal::new(
                        "duplicates_invalid",
                        "staged duplicate groups without their volume",
                    )
                })?)
                .map_err(|e| Refusal::new("duplicates_invalid", format!("selection: {e}")))?;
            let groups = spec["groups"].as_array_mut();
            match groups {
                Some(list) => list.extend(staged.groups.into_iter().map(serde_json::Value::from)),
                None => spec["groups"] = serde_json::Value::from(staged.groups),
            }
            duplicates = Some(spec.to_string());
        }
    }
    // A plan previews a mutation: the switch that stops apply stops it too.
    let config = guard::read_cleanup_config()
        .map_err(|e| Refusal::new("cleanup_disabled", e.to_string()))?;
    let mut keep = Vec::new();
    let mut summary = None;
    // The group each duplicates path is a copy of, for its refusals.
    let mut group_of: HashMap<Vec<u8>, String> = HashMap::new();
    if let Some(spec) = duplicates.as_deref() {
        let expansion = crate::duplicates::command::expand(spec, state_dir)?;
        for (path, group) in expansion.paths.iter().zip(expansion.groups) {
            group_of
                .entry(path.as_os_str().as_bytes().to_vec())
                .or_insert(group);
        }
        targets
            .operands
            .extend(expansion.paths.into_iter().map(PathBuf::into_os_string));
        keep = expansion.keep;
        summary = Some(expansion.summary);
    }
    let env = Environment::load()?;
    let (checked, mut errors) = check_targets(&env, &targets)?;
    let expected: HashMap<&[u8], u64> = expected
        .iter()
        .map(|e| (e.path.as_slice(), e.expect.objectid))
        .collect();
    for (_, target) in &checked {
        if expected
            .get(target.path.as_slice())
            .is_some_and(|&objectid| objectid != target.inode)
        {
            errors.push(TargetError::new(
                JsonPath::new(&target.path),
                "target_changed",
                "target_changed: another item is at this path since the scan; rescan".to_string(),
            ));
        }
    }
    if let Some(first) = checked.first() {
        let fsid = first.0.fsid;
        if let Some((other, _)) = checked.iter().find(|(r, _)| r.fsid != fsid) {
            errors.push(TargetError::new(
                JsonPath::new(other.path.as_os_str().as_bytes()),
                "selection_boundary",
                "a plan moves items of one volume only".to_string(),
            ));
        }
    }
    for (_, target) in &checked {
        if let Err(refusal) = within_allowed_roots(&config, &target.path) {
            errors.push(TargetError::new(
                JsonPath::new(&target.path),
                refusal.code,
                refusal.message,
            ));
        }
    }
    if !errors.is_empty() {
        name_groups(&mut errors, &group_of);
        return Err(Box::new(TargetErrors(errors)));
    }
    let Some((first, _)) = checked.first() else {
        return Err(Refusal::new("nothing_selected", "no path selected").into());
    };
    let (fsid, mount, filesystem) = (first.fsid, first.mount.clone(), first.filesystem);
    let mut checked = checked;
    size_targets(state_dir, &mut checked)?;
    // One group per shared folder, in the order first selected.
    let mut shares: Vec<PlanShare> = Vec::new();
    let mut statuses: Vec<(Vec<u8>, BinStatus)> = Vec::new();
    let mut planned = Vec::new();
    // Folders below the volume DSM's mount list lacks, each confirmed
    // with DSM once before its items are planned to be removed for good.
    let mut outside: Vec<Vec<u8>> = Vec::new();
    let mount_bytes = mount.as_os_str().as_bytes();
    for (_, mut target) in checked {
        let status = match statuses.iter().find(|(s, _)| *s == target.share) {
            Some((_, status)) => *status,
            // No shared folder, so no Recycle Bin to ask DSM about; but a
            // share the mount list missed must never lose items for good.
            None if target.delete => {
                if !outside.contains(&target.share) {
                    if moves::dsm_knows_share(tools, &target.share)? {
                        return Err(Refusal::new(
                            "recycle_bin_unavailable",
                            format!(
                                "recycle_bin_unavailable: DSM knows {} as a shared folder but does not list it as mounted on this volume; nothing is removed for good",
                                crate::output::escape_human(&target.share)
                            ),
                        )
                        .into());
                    }
                    outside.push(target.share.clone());
                }
                BinStatus::default()
            }
            None => {
                let status = moves::bin_status(tools, &target.share)?;
                statuses.push((target.share.clone(), status));
                status
            }
        };
        // A shared folder without a Recycle Bin takes the same way as a
        // folder that is no shared folder: its items are removed for good,
        // only once the user confirms that.
        let reason = if target.delete {
            Some(DeleteReason::NoSharedFolder)
        } else if !status.enabled {
            target.delete = true;
            Some(DeleteReason::RecycleBinOff)
        } else {
            None
        };
        let item = PlanItem {
            path: JsonPath::new(&target.path),
            kind: target.kind,
            size: target.size,
            symlinks: target.symlinks,
            drive: moves::in_drive(&target.path, mount_bytes),
        };
        let root = [mount_bytes, b"/", &target.share].concat();
        match shares.iter_mut().find(|s| s.share.bytes() == target.share) {
            Some(share) => {
                share.size = share.size.plus(target.size);
                share.symlinks += target.symlinks;
                share.item_count += 1;
                share.drive_items += u64::from(item.drive);
                if share.items.len() < PREVIEW_ITEMS {
                    share.items.push(item);
                }
            }
            None => shares.push(PlanShare {
                share: JsonPath::new(&target.share),
                path: JsonPath::new(&root),
                recycle_bin: status,
                size: target.size,
                symlinks: target.symlinks,
                item_count: 1,
                drive_items: u64::from(item.drive),
                items: vec![item],
                delete: target.delete,
                delete_reason: reason,
            }),
        }
        planned.push(target);
    }
    let now = SystemTime::now();
    let mut envelope = Envelope::new("cleanup-plan", now);
    if planned.iter().any(|t| !t.delete) && !moves::recyclebind_running(tools) {
        return Err(Refusal::new(
            "recycle_bin_unavailable",
            "recycle_bin_unavailable: DSM's recyclebind service is not running",
        )
        .into());
    }
    let deletes = planned.iter().filter(|t| t.delete).count() as u64;
    if deletes > 0 {
        envelope.warnings.push(Notice::new(
            "deletes_for_good",
            format!(
                "{deletes} item(s) have no Recycle Bin (in no shared folder, or in a shared folder whose Recycle Bin is off): apply removes them for good, only with --confirm-delete; to move them to a Recycle Bin instead, turn it on ({}) and make a new plan",
                moves::ENABLE_ROUTE
            ),
        ));
    }
    let created = unix_seconds(now);
    let plan = Plan {
        format: moves::PLAN_FORMAT,
        plan_id: guard::new_plan_id(now, std::process::id()),
        created_at: created,
        expires_at: created + moves::PLAN_LIFETIME_SECONDS,
        volume: mount_bytes.to_vec(),
        filesystem_uuid: crate::resolve::fsid_text(&fsid),
        filesystem,
        targets: planned,
        keep,
        checksum: String::new(),
    }
    .seal();
    let uid = sys::effective_uid();
    let paths = cleanup_state_paths(state_dir, uid)?;
    let internal = paths.plans.join(format!("{}.json", plan.plan_id));
    write_private_plan(&internal, &plan, uid)?;
    let shown = match output_path {
        Some(path) => {
            write_private_plan(&path, &plan, uid)?;
            path
        }
        None => internal,
    };
    Ok(PlanDocument {
        envelope,
        plan_id: plan.plan_id.clone(),
        plan_file: JsonPath::new(shown.as_os_str().as_bytes()),
        expires_at: crate::time::rfc3339(
            std::time::UNIX_EPOCH + Duration::from_secs(plan.expires_at),
        ),
        items: plan.targets.len() as u64,
        deletes,
        paths: plan
            .targets
            .iter()
            .take(PREVIEW_ITEMS)
            .map(|t| JsonPath::new(&t.path))
            .collect(),
        size: plan.size(),
        shares,
        duplicates: summary,
    })
}

// ---- Staged selections ------------------------------------------------

/// Where `cleanup select` stages selections, below root's state on disk.
fn selections_dir(state_dir: &Path, uid: u32) -> Result<PathBuf, Failure> {
    guard::check_state_ancestors(state_dir, uid)?;
    let base = state_dir.join("cleanup");
    let dir = base.join("selections");
    for path in [&base, &dir] {
        state::private_dir(path, uid)?;
    }
    Ok(dir)
}

/// A staged selection's ID: `selection-<hex>-<hex>`, never a path.
pub fn valid_selection_id(value: &str) -> bool {
    value.strip_prefix("selection-").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 48
            && rest.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    })
}

/// Staged selections older than this are removed when another starts.
const SELECTION_KEPT_SECONDS: u64 = 24 * 3600;

/// One staged entry, a JSON line: a path with its report inode, or a
/// duplicate group.
#[derive(Debug, Serialize, Deserialize)]
struct StagedEntry {
    #[serde(
        default,
        rename = "p",
        skip_serializing_if = "Option::is_none",
        with = "staged_hex"
    )]
    path: Option<Vec<u8>>,
    #[serde(default, rename = "o", skip_serializing_if = "Option::is_none")]
    objectid: Option<u64>,
    #[serde(default, rename = "g", skip_serializing_if = "Option::is_none")]
    group: Option<String>,
}

mod staged_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(bytes) => crate::report::hex_bytes::serialize(bytes, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let text = String::deserialize(d)?;
        crate::report::hex_bytes::decode(&text)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("not hex"))
    }
}

/// What one `cleanup select` adds: paths, each with the report inode it
/// was selected with, and duplicate group IDs (the `webapi` checked them).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionChunk {
    #[serde(default)]
    pub paths: Vec<StagedPath>,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedPath {
    #[serde(with = "crate::report::hex_bytes")]
    pub path: Vec<u8>,
    #[serde(default)]
    pub objectid: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SelectDocument {
    #[serde(flatten)]
    envelope: Envelope,
    selection: String,
    /// Paths and groups staged so far.
    paths: u64,
    groups: u64,
}

/// `cleanup select`: adds a chunk to a staged selection, a new one when
/// `id` is `None`, so a selection of any size reaches `cleanup plan`
/// without passing through a request or an argv whole.
pub fn cleanup_select(
    id: Option<String>,
    chunk: &str,
    json: bool,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let result = stage_chunk(id, chunk, json, state_dir);
    finish_cleanup(json, "cleanup-select", b"", result)
}

fn stage_chunk(
    id: Option<String>,
    chunk: &str,
    json: bool,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let chunk: SelectionChunk = serde_json::from_str(chunk)
        .map_err(|e| Refusal::new("invalid_selection", format!("selection: {e}")))?;
    let uid = sys::effective_uid();
    let dir = selections_dir(state_dir, uid)?;
    let (id, file) = match id {
        Some(id) => {
            if !valid_selection_id(&id) {
                return Err(Refusal::new("invalid_selection", "not a selection ID").into());
            }
            let file = std::fs::OpenOptions::new()
                .append(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(dir.join(&id))
                .map_err(|_| {
                    Refusal::new(
                        "selection_gone",
                        "the staged selection is gone: select again",
                    )
                })?;
            let meta = file.metadata()?;
            if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
                return Err(
                    Refusal::new("selection_gone", "the staged selection is not private").into(),
                );
            }
            (id, file)
        }
        None => {
            prune_selections(&dir);
            let now = SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let id = format!("selection-{now:x}-{:x}", std::process::id());
            let file = std::fs::OpenOptions::new()
                .append(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(dir.join(&id))?;
            (id, file)
        }
    };
    let mut text = Vec::new();
    for path in chunk.paths {
        serde_json::to_writer(
            &mut text,
            &StagedEntry {
                path: Some(path.path),
                objectid: path.objectid,
                group: None,
            },
        )?;
        text.push(b'\n');
    }
    for group in chunk.groups {
        serde_json::to_writer(
            &mut text,
            &StagedEntry {
                path: None,
                objectid: None,
                group: Some(group),
            },
        )?;
        text.push(b'\n');
    }
    // One write of whole lines: an interrupted chunk is never half a line
    // short of the next.
    (&file).write_all(&text)?;
    file.sync_data()?;
    // Count the lines staged so far without parsing them again.
    let (mut paths, mut groups) = (0u64, 0u64);
    let mut lines = std::io::BufReader::new(guard::open_absolute_no_follow(&dir.join(&id))?);
    let mut line = Vec::new();
    while std::io::BufRead::read_until(&mut lines, b'\n', &mut line)? > 0 {
        if line.starts_with(br#"{"g""#) {
            groups += 1;
        } else if line.starts_with(br#"{"p""#) {
            paths += 1;
        }
        line.clear();
    }
    let doc = SelectDocument {
        envelope: Envelope::new("cleanup-select", SystemTime::now()),
        selection: id,
        paths,
        groups,
    };
    emit(json, &doc, || {
        format!(
            "Selection {}: {} path(s), {} duplicate group(s)\n",
            doc.selection, doc.paths, doc.groups
        )
    })?;
    Ok(0)
}

fn prune_selections(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age.as_secs() > SELECTION_KEPT_SECONDS);
        if old && valid_selection_id(&entry.file_name().to_string_lossy()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A staged selection as `cleanup plan` reads it.
#[derive(Debug, Default)]
struct Staged {
    paths: Vec<Vec<u8>>,
    expected: Vec<ExpectedPath>,
    groups: Vec<String>,
}

fn read_staged(file: &Path) -> Result<Staged, Failure> {
    let file = guard::private_file(guard::open_absolute_no_follow(file)?, sys::effective_uid())?;
    let mut staged = Staged::default();
    for line in std::io::BufRead::lines(std::io::BufReader::new(file)) {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let entry: StagedEntry = serde_json::from_str(&line)
            .map_err(|e| Refusal::new("invalid_selection", format!("staged selection: {e}")))?;
        match (entry.path, entry.group) {
            (Some(path), None) => {
                if let Some(objectid) = entry.objectid {
                    staged.expected.push(ExpectedPath {
                        path: path.clone(),
                        expect: ExpectedIdentity {
                            objectid,
                            rest: serde_json::Map::new(),
                        },
                    });
                }
                staged.paths.push(path);
            }
            (None, Some(group)) => staged.groups.push(group),
            _ => {
                return Err(
                    Refusal::new("invalid_selection", "staged selection: a bad line").into(),
                );
            }
        }
    }
    Ok(staged)
}

/// Reads the staged selection `id` once: a plan uses it up.
fn take_selection(state_dir: &Path, id: &str) -> Result<Staged, Failure> {
    if !valid_selection_id(id) {
        return Err(Refusal::new("invalid_selection", "not a selection ID").into());
    }
    let file = selections_dir(state_dir, sys::effective_uid())?.join(id);
    let staged = read_staged(&file).map_err(|_| {
        Refusal::new(
            "selection_gone",
            "the staged selection is gone: select again",
        )
    })?;
    let _ = std::fs::remove_file(&file);
    Ok(staged)
}

/// Several refused paths: each is one `errors[]` item; the first one is
/// the text.
#[derive(Debug)]
pub struct TargetErrors(pub Vec<TargetError>);

impl std::fmt::Display for TargetErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let lines: Vec<&str> = self.0.iter().map(|e| e.message.as_str()).collect();
        f.write_str(&lines.join("\n"))
    }
}

impl std::error::Error for TargetErrors {}

fn render_plan(doc: &PlanDocument) -> String {
    let mut out = String::new();
    for share in &doc.shares {
        let name = crate::output::escape_human(&share.share.bytes());
        if share.delete_reason == Some(DeleteReason::RecycleBinOff) {
            out.push_str(&format!(
                "Shared folder {name} has no Recycle Bin: {} item(s), {} are REMOVED FOR GOOD, with no way to restore them; to move them to its Recycle Bin instead, turn it on ({}) and make a new plan\n",
                share.item_count,
                crate::output::human_bytes(share.size.allocated_bytes),
                moves::ENABLE_ROUTE
            ));
        } else if share.delete {
            out.push_str(&format!(
                "Folder {name} is no shared folder and has no Recycle Bin: {} item(s), {} are REMOVED FOR GOOD, with no way to restore them\n",
                share.item_count,
                crate::output::human_bytes(share.size.allocated_bytes),
            ));
        } else {
            out.push_str(&format!(
                "Shared folder {name}: {} item(s), {} move to its Recycle Bin{}\n",
                share.item_count,
                crate::output::human_bytes(share.size.allocated_bytes),
                if share.recycle_bin.admin_only {
                    " (only administrators can restore them)"
                } else {
                    " (every user of the shared folder can see and restore them)"
                }
            ));
        }
        for item in &share.items {
            out.push_str(&format!(
                "  {}{}\n",
                crate::output::escape_human(&item.path.bytes()),
                if item.drive {
                    " (Synology Drive: clients sync the move)"
                } else {
                    ""
                }
            ));
        }
        if share.item_count > share.items.len() as u64 {
            out.push_str(&format!(
                "  and {} more\n",
                share.item_count - share.items.len() as u64
            ));
        }
    }
    out.push_str(&format!(
        "Will move to the Recycle Bin: {} item(s), {}; empty the Recycle Bin to free the space\nPlan: {}\n",
        doc.items,
        crate::output::human_bytes(doc.size.allocated_bytes),
        crate::output::escape_human(&doc.plan_file.bytes())
    ));
    if doc.deletes > 0 {
        out.push_str(&format!(
            "Of them, {} item(s) are removed for good instead: apply needs --confirm-delete\n",
            doc.deletes
        ));
    }
    out
}

fn write_private_plan(path: &Path, plan: &Plan, uid: u32) -> Result<(), Failure> {
    let parent = path.parent().ok_or("plan path has no parent directory")?;
    state::private_dir(parent, uid)?;
    let directory = guard::open_absolute_no_follow(parent)?;
    let name = path.file_name().ok_or("plan path has no file name")?;
    let mut file = sys::openat(
        directory.as_fd(),
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o600,
    )?;
    // Buffered: a plan of a large duplicates selection keeps tens of
    // thousands of copies, and serde_json writes token by token.
    let mut out = std::io::BufWriter::new(&mut file);
    serde_json::to_writer(&mut out, plan)?;
    out.write_all(b"\n")?;
    out.flush()?;
    drop(out);
    file.sync_all()?;
    directory.sync_all()?;
    Ok(())
}

/// A plan of over two million items; a plan is about 100 bytes an item.
const MAX_PLAN_BYTES: u64 = 256 << 20;

fn read_plan(path: &Path) -> Result<Plan, Failure> {
    let file = guard::open_absolute_no_follow(path)?;
    if file.metadata()?.len() > MAX_PLAN_BYTES {
        return Err(Refusal::stale("plan is too large").into());
    }
    // Buffered: unbuffered, serde_json reads a byte per system call, and
    // a plan of a large duplicates selection (its kept copies) runs to
    // tens of MB: about 12 s on a test NAS before the job was even queued.
    let plan: Plan = serde_json::from_reader(std::io::BufReader::new(file))
        .map_err(|e| Refusal::stale(format!("not a plan of this version: {e}")))?;
    if !valid_plan_id(&plan.plan_id) {
        return Err(Refusal::stale("plan has an invalid id").into());
    }
    Ok(plan)
}

pub(super) struct CleanupStatePaths {
    pub(super) plans: PathBuf,
    pub(super) logs: PathBuf,
}

pub(super) fn cleanup_state_paths(
    state_dir: &Path,
    uid: u32,
) -> Result<CleanupStatePaths, Failure> {
    guard::check_state_ancestors(state_dir, uid)?;
    let base = state_dir.join("cleanup");
    let paths = CleanupStatePaths {
        plans: base.join("plans"),
        logs: base.join("logs"),
    };
    for path in [&base, &paths.plans, &paths.logs] {
        state::private_dir(path, uid)?;
    }
    Ok(paths)
}

pub fn valid_plan_id(value: &str) -> bool {
    value.strip_prefix("cleanup-").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

/// The plan at `path` once the checks that need no target pass: root,
/// an intact plan in time, cleanup turned on, and its private copy in
/// root's state (what a detached job runs).
fn plan_to_apply(path: &Path, state_dir: &Path) -> Result<(Plan, PathBuf), Failure> {
    if sys::effective_uid() != 0 {
        return Err("cleanup apply requires root privileges".into());
    }
    let plan = read_plan(path)?;
    plan.validate(guard::now_seconds())?;
    let config = guard::read_cleanup_config()
        .map_err(|e| Refusal::new("cleanup_disabled", e.to_string()))?;
    for target in &plan.targets {
        within_allowed_roots(&config, &target.path)?;
    }
    let uid = sys::effective_uid();
    let paths = cleanup_state_paths(state_dir, uid)?;
    let internal = paths.plans.join(format!("{}.json", plan.plan_id));
    match read_plan(&internal) {
        Ok(existing) if existing == plan => {}
        Ok(_) => return Err(Refusal::stale("plan id belongs to another plan").into()),
        Err(_) if internal.exists() => {
            return Err(Refusal::stale("private plan copy unreadable").into());
        }
        Err(_) => write_private_plan(&internal, &plan, uid)?,
    }
    Ok((plan, internal))
}

/// One item of an apply document.
#[derive(Debug, Serialize)]
struct AppliedTarget {
    path: JsonPath,
    share: JsonPath,
    outcome: Outcome,
    /// What is in the Recycle Bin now, as allocated when planned.
    moved_bytes: u64,
    remaining_entries: u64,
    remaining_bytes: u64,
    /// False when the count of what stayed stopped at its bound
    /// (100,000 entries): the remaining values are then lower bounds.
    remaining_complete: bool,
    /// Up to 50 paths still at the item's place.
    remaining: Vec<JsonPath>,
    detail: Option<String>,
}

/// What one shared folder's Recycle Bin received.
#[derive(Debug, Serialize)]
struct BinReceived {
    share: JsonPath,
    path: JsonPath,
    admin_only: bool,
    items: u64,
    moved_bytes: u64,
}

#[derive(Debug, Serialize)]
struct ApplyDocument {
    #[serde(flatten)]
    envelope: Envelope,
    plan_id: String,
    moved: u64,
    /// Removed for good: no Recycle Bin took them.
    deleted: u64,
    already_absent: u64,
    partial: u64,
    changed: u64,
    failed: u64,
    /// The space now in Recycle Bins: freed only once they are emptied.
    moved_bytes: u64,
    /// The space of what was removed for good: freed now (on Btrfs once
    /// no snapshot or reflink holds it).
    deleted_bytes: u64,
    /// Items in the plan.
    items: u64,
    /// The first [`LISTED_TARGETS`] items that did not move (moved only
    /// partly, changed, failed, or their Recycle Bin is off): the counts
    /// above cover the rest.
    targets: Vec<AppliedTarget>,
    recycle_bins: Vec<BinReceived>,
    report_state_stale: bool,
    rescan: Vec<JsonPath>,
    space_release: &'static str,
}

/// Items that did not move an apply lists, with their errors.
const LISTED_TARGETS: usize = 1000;

pub const SPACE_RELEASE: &str = "moved items stay on the volume in their shared folder's Recycle Bin: their space is freed after the Recycle Bin is emptied (Control Panel > Shared Folder > Action > Empty Recycle Bin, or a Recycle Bin task in Task Scheduler), then the Btrfs commit and cleaner";

#[allow(clippy::too_many_arguments)]
pub fn cleanup_apply(
    path: &Path,
    yes: bool,
    json: bool,
    profile: crate::resource::Profile,
    detach: Option<Launcher>,
    origin: ApplyOrigin,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let subject = path.as_os_str().as_bytes();
    if let Some(launcher) = detach {
        let result = detach_apply(
            path,
            profile,
            origin.requested_by,
            origin.confirm_delete,
            launcher,
            state_dir,
        );
        return finish_cleanup(json, "cleanup-apply", subject, result);
    }
    let attempt = origin.job_id.map_or_else(new_apply_attempt, str::to_string);
    let result = apply_plan(
        "cleanup-apply",
        path,
        (yes, origin.confirm_delete),
        json,
        profile,
        state_dir,
        &attempt,
        &Tools::dsm(),
    );
    match result {
        Err(error) if origin.job_id.is_some() => {
            let mut doc = cleanup_failure_document(
                "cleanup-apply",
                SystemTime::now(),
                subject,
                error.as_ref(),
            );
            let exit_code = guard::failure_exit_code(error.as_ref());
            if let Err(e) = end_queued_job(&state_dir.join("jobs"), &attempt, &mut doc, exit_code) {
                eprintln!("synoreclaim: warning: could not record the cleanup job's end: {e}");
            }
            if json {
                emit(true, &doc, String::new)?;
                Ok(exit_code)
            } else {
                report_refusal(Err(error))
            }
        }
        result => finish_cleanup(json, "cleanup-apply", subject, result),
    }
}

pub(super) fn new_apply_attempt() -> String {
    guard::new_plan_id(SystemTime::now(), std::process::id())
}

fn cleanup_job(id: &str, plan: &Plan, profile: resource::Profile, now: SystemTime) -> Job {
    // The job names the shared folders it moves from, not each item:
    // jobs.list stays small for a plan of any size.
    let mut targets: Vec<Vec<u8>> = plan
        .targets
        .iter()
        .map(|t| [plan.volume.as_slice(), b"/", &t.share].concat())
        .collect();
    targets.sort();
    targets.dedup();
    let filesystem = jobs::JobFilesystem {
        fsid: plan.filesystem_uuid.clone(),
        mount: plan.volume.clone(),
    };
    let mut job = Job::cleanup_apply(id, &targets, filesystem, profile, unix_seconds(now));
    job.plan_id = Some(plan.plan_id.clone());
    job
}

fn detach_apply(
    path: &Path,
    profile: resource::Profile,
    requested_by: Option<&str>,
    confirm_delete: bool,
    launcher: Launcher,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let (plan, internal) = plan_to_apply(path, state_dir)?;
    deletes_confirmed(&plan, confirm_delete)?;
    let uid = sys::effective_uid();
    let store = Store::open(state_dir, uid)?;
    let jobs_dir = jobs_dir(&store);
    let now = SystemTime::now();
    let id = new_apply_attempt();
    let mut job = cleanup_job(&id, &plan, profile, now);
    job.requested_by = requested_by.map(str::to_string);
    jobs::prune(&jobs_dir, jobs::FINISHED_KEPT)?;
    jobs::queue(&jobs_dir, uid, job)?;
    let mut run = vec![std::env::current_exe()?.into_os_string()];
    run.extend(["cleanup", "apply", "--yes", "--json"].map(std::ffi::OsString::from));
    if confirm_delete {
        run.push("--confirm-delete".into());
    }
    run.push(format!("--profile={}", profile.name()).into());
    if let Some(user) = requested_by {
        run.push(format!("--requested-by={user}").into());
    }
    run.push(format!("--job-id={id}").into());
    let mut state = std::ffi::OsString::from("--state-dir=");
    state.push(store.root());
    run.push(state);
    run.push("--".into());
    run.push(internal.into());
    let mut doc = DetachDocument::new(Envelope::new("detach", now), launcher);
    doc.envelope.job_id = Some(id.clone());
    if let Err(e) = hand_off(&mut doc, &jobs_dir.join(&id), &run, launcher)? {
        let now = unix_seconds(SystemTime::now());
        Live::launch(&jobs_dir, uid, &id, now)?.finish(
            JobState::Failed,
            Some("launch_failed"),
            now,
        )?;
        return Err(format!("could not launch job {id}: {e}").into());
    }
    Ok(0)
}

/// Refuses a plan that removes items for good unless the user confirmed
/// exactly that (`--confirm-delete`; `confirm_delete` in the API): `--yes`
/// alone confirms only moves into a Recycle Bin.
fn deletes_confirmed(plan: &Plan, confirm_delete: bool) -> Result<(), Refusal> {
    let (count, _) = plan.deletes();
    if count > 0 && !confirm_delete {
        return Err(Refusal::new(
            "delete_not_confirmed",
            format!(
                "delete_not_confirmed: {count} item(s) have no Recycle Bin and would be removed for good; confirm with --confirm-delete"
            ),
        ));
    }
    Ok(())
}

fn confirm(yes: bool, plan: &Plan) -> Result<bool, Failure> {
    let (deletes, deleted) = plan.deletes();
    let moves = plan.size().allocated_bytes - deleted.allocated_bytes;
    eprintln!(
        "Will move to the Recycle Bin: {} item(s), {}; empty the shared folder's Recycle Bin to free the space",
        plan.targets.len() - deletes,
        crate::output::human_bytes(moves)
    );
    if deletes > 0 {
        eprintln!(
            "Will REMOVE FOR GOOD, with no way to restore: {deletes} item(s), {}; they have no Recycle Bin",
            crate::output::human_bytes(deleted.allocated_bytes)
        );
    }
    if yes {
        return Ok(true);
    }
    if !sys::stdin_is_terminal() {
        eprintln!("synoreclaim: noninteractive cleanup requires --yes");
        return Ok(false);
    }
    eprint!("Move them to the Recycle Bin? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

#[allow(clippy::too_many_arguments)]
fn apply_plan(
    command: &'static str,
    path: &Path,
    (yes, confirm_delete): (bool, bool),
    json: bool,
    profile: resource::Profile,
    state_dir: &Path,
    attempt: &str,
    tools: &Tools,
) -> Result<i32, Failure> {
    let (plan, _) = plan_to_apply(path, state_dir)?;
    // A duplicates cleanup keeps a copy of each group: refuse before
    // anything moves when a kept copy went or changed size.
    moves::check_keepers(&plan.keep)?;
    deletes_confirmed(&plan, confirm_delete)?;
    if plan.targets.iter().any(|t| !t.delete) && !moves::recyclebind_running(tools) {
        return Err(Refusal::new(
            "recycle_bin_unavailable",
            "recycle_bin_unavailable: DSM's recyclebind service is not running",
        )
        .into());
    }
    if !confirm(yes, &plan)? {
        return Err(Refusal::new("not_confirmed", "cleanup not confirmed").into());
    }
    sys::install_stop_handlers()?;
    let uid = sys::effective_uid();
    let store = Store::open(state_dir, uid)?;
    let holder = state::LockHolder::new(Some(attempt), "cleanup-apply");
    let _lock = match state::lock_filesystem(
        Path::new(state::LOCK_DIR),
        &plan.filesystem_uuid,
        uid,
        &holder,
    ) {
        Ok(lock) => lock,
        Err(state::StateError::Busy { .. }) => {
            return Err(Refusal::new("busy", "fs writer lock is busy").into());
        }
        Err(error) => return Err(error.into()),
    };
    let _mutation_locks = state::lock_mutation_paths(
        Path::new(state::LOCK_DIR),
        &plan.filesystem_uuid,
        &moves::os_path(&plan.volume),
        &locked_paths(&plan),
        uid,
        &holder,
    )?;
    let paths = cleanup_state_paths(state_dir, uid)?;
    let result_path = paths.logs.join(format!("{}-{attempt}.json", plan.plan_id));
    let mut job = ApplyJob::start(&jobs_dir(&store), uid, attempt, &plan, profile)?;
    // The reports describe the items before the move: mark them before
    // the first one moves, as the record says what ran.
    let report_state_stale = match invalidate(state_dir, uid, &plan, &result_path) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("synoreclaim: warning: could not mark reports out of date: {error}");
            false
        }
    };
    let Moves {
        results,
        cancelled,
        stopped,
    } = move_all(tools, &plan, &mut job);
    let mut document = apply_document(command, &plan, &results, report_state_stale);
    document.envelope.job_id = Some(attempt.to_string());
    let mut status = apply_status(&results, plan.targets.len());
    if cancelled {
        document.envelope.completion = Completion::Cancelled;
        document.envelope.warnings.push(Notice::new(
            "cancelled",
            "cancel requested: stopped after the current item; the rest stays in place",
        ));
        status = Completion::Cancelled.exit_code(None);
    } else if let Some(signal) = stopped {
        document.envelope.completion = Completion::Paused;
        status = Completion::Paused.exit_code(Some(signal));
    } else {
        document.envelope.completion = match status {
            0 => Completion::Complete,
            _ => Completion::Partial,
        };
    }
    if let Err(e) = record_moves(&paths, &plan, &results) {
        eprintln!("synoreclaim: warning: could not record what moved: {e}");
    }
    if let Err(e) = save_result(&result_path, &document) {
        eprintln!("synoreclaim: warning: could not keep the cleanup result: {e}");
    }
    if let Err(error) = job.finish(&plan, &document, status) {
        eprintln!("synoreclaim: warning: could not record the cleanup job's end: {error}");
    }
    emit(json, &document, || render_apply(&document))?;
    Ok(status)
}

/// What the moves of a plan came to.
struct Moves {
    results: Vec<moves::Moved>,
    cancelled: bool,
    stopped: Option<i32>,
}

/// Moves the plan's items one by one, until the end, a cancel or a stop
/// signal, with the job's progress as they go.
fn move_all(tools: &Tools, plan: &Plan, job: &mut ApplyJob) -> Moves {
    let mut results: Vec<moves::Moved> = Vec::with_capacity(plan.targets.len());
    let mut cancelled = false;
    let mut stopped = None;
    let mut bins = moves::BinCache::default();
    for target in &plan.targets {
        if job.cancel_requested() {
            cancelled = true;
            break;
        }
        if let Some(signal) = sys::stop_signal() {
            stopped = Some(signal);
            break;
        }
        let moved = {
            let mut control = JobControl { job, plan };
            if target.delete {
                moves::delete_target(target, &mut control)
            } else {
                moves::move_target(tools, target, &mut bins, &mut control)
            }
        };
        let stop = moved.cancelled;
        job.ended(plan, &moved);
        results.push(moved);
        if stop {
            match sys::stop_signal() {
                Some(signal) => stopped = Some(signal),
                None => cancelled = true,
            }
            break;
        }
    }
    Moves {
        results,
        cancelled,
        stopped,
    }
}

/// Items one apply locks one by one; a larger plan locks its shared
/// folders instead, so a plan of any size holds a few locks.
const LOCKED_ITEMS: usize = 256;

/// What an apply locks against other mutations: its items, or the
/// shared folders of a plan of more than [`LOCKED_ITEMS`] items.
fn locked_paths(plan: &Plan) -> Vec<PathBuf> {
    if plan.targets.len() <= LOCKED_ITEMS {
        return plan
            .targets
            .iter()
            .map(|t| moves::os_path(&t.path))
            .collect();
    }
    let mut shares: Vec<Vec<u8>> = plan
        .targets
        .iter()
        .map(|t| [plan.volume.as_slice(), b"/", &t.share].concat())
        .collect();
    shares.sort();
    shares.dedup();
    shares.iter().map(|p| moves::os_path(p)).collect()
}

/// 0 when every item moved or was already gone; 4 when one changed; 3
/// when one moved only partly, failed, or its Recycle Bin went off.
fn apply_status(results: &[moves::Moved], planned: usize) -> i32 {
    if results.iter().any(|r| r.outcome == Outcome::Changed) {
        4
    } else if results.len() < planned
        || results.iter().any(|r| {
            !matches!(
                r.outcome,
                Outcome::Moved | Outcome::Deleted | Outcome::AlreadyAbsent
            )
        })
    {
        3
    } else {
        0
    }
}

fn apply_document(
    command: &'static str,
    plan: &Plan,
    results: &[moves::Moved],
    report_state_stale: bool,
) -> ApplyDocument {
    let count = |o: Outcome| results.iter().filter(|r| r.outcome == o).count() as u64;
    let mut recycle_bins: Vec<BinReceived> = Vec::new();
    let mut targets = Vec::new();
    for (target, result) in plan.targets.iter().zip(results) {
        let root = [plan.volume.as_slice(), b"/", &target.share].concat();
        if result.moved.entries > 0 && !result.for_good {
            match recycle_bins
                .iter_mut()
                .find(|b| b.share.bytes() == target.share)
            {
                Some(bin) => {
                    bin.items += 1;
                    bin.moved_bytes += result.moved.allocated_bytes;
                }
                None => recycle_bins.push(BinReceived {
                    share: JsonPath::new(&target.share),
                    path: JsonPath::new(&root),
                    admin_only: false,
                    items: 1,
                    moved_bytes: result.moved.allocated_bytes,
                }),
            }
        }
        if matches!(
            result.outcome,
            Outcome::Moved | Outcome::Deleted | Outcome::AlreadyAbsent
        ) || targets.len() >= LISTED_TARGETS
        {
            continue;
        }
        targets.push(AppliedTarget {
            path: JsonPath::new(&target.path),
            share: JsonPath::new(&target.share),
            outcome: result.outcome,
            moved_bytes: result.moved.allocated_bytes,
            remaining_entries: result.remaining.entries,
            remaining_bytes: result.remaining.allocated_bytes,
            remaining_complete: result.remaining_complete,
            remaining: result
                .remaining_paths
                .iter()
                .map(|p| JsonPath::new(p))
                .collect(),
            detail: result.detail.clone(),
        });
    }
    for bin in &mut recycle_bins {
        bin.admin_only = moves::bin_status(&Tools::dsm(), &bin.share.bytes())
            .map(|s| s.admin_only)
            .unwrap_or(false);
    }
    let mut envelope = Envelope::new(command, SystemTime::now());
    for (target, result) in plan
        .targets
        .iter()
        .zip(results)
        .filter(|(_, r)| {
            !matches!(
                r.outcome,
                Outcome::Moved | Outcome::Deleted | Outcome::AlreadyAbsent
            )
        })
        .take(LISTED_TARGETS)
    {
        let code = match result.outcome {
            Outcome::Moved | Outcome::Deleted | Outcome::AlreadyAbsent => continue,
            Outcome::Partial => "moved_partly",
            Outcome::Changed => "target_changed",
            Outcome::Failed => "move_failed",
        };
        envelope.errors.push(TargetError::new(
            JsonPath::new(&target.path),
            code,
            result.detail.clone().unwrap_or_else(|| code.to_string()),
        ));
    }
    let mut rescan: Vec<Vec<u8>> = plan
        .targets
        .iter()
        .map(|t| [plan.volume.as_slice(), b"/", &t.share].concat())
        .collect();
    rescan.sort();
    rescan.dedup();
    ApplyDocument {
        envelope,
        plan_id: plan.plan_id.clone(),
        moved: count(Outcome::Moved),
        deleted: count(Outcome::Deleted),
        already_absent: count(Outcome::AlreadyAbsent),
        partial: count(Outcome::Partial),
        changed: count(Outcome::Changed),
        failed: count(Outcome::Failed),
        moved_bytes: plan
            .targets
            .iter()
            .zip(results)
            .filter(|(_, r)| !r.for_good)
            .map(|(_, r)| r.moved.allocated_bytes)
            .sum(),
        deleted_bytes: plan
            .targets
            .iter()
            .zip(results)
            .filter(|(_, r)| r.for_good)
            .map(|(_, r)| r.moved.allocated_bytes)
            .sum(),
        items: plan.targets.len() as u64,
        targets,
        recycle_bins,
        report_state_stale,
        rescan: rescan.iter().map(|p| JsonPath::new(p)).collect(),
        space_release: SPACE_RELEASE,
    }
}

fn render_apply(doc: &ApplyDocument) -> String {
    let mut out = format!(
        "Moved to the Recycle Bin: {} item(s), {}; space is freed after emptying it\n",
        doc.moved + doc.partial,
        crate::output::human_bytes(doc.moved_bytes)
    );
    if doc.deleted > 0 {
        out.push_str(&format!(
            "Removed for good (no Recycle Bin): {} item(s), {}\n",
            doc.deleted,
            crate::output::human_bytes(doc.deleted_bytes)
        ));
    }
    for target in &doc.targets {
        if matches!(
            target.outcome,
            Outcome::Moved | Outcome::Deleted | Outcome::AlreadyAbsent
        ) {
            continue;
        }
        out.push_str(&format!(
            "{}: {:?}{}\n",
            crate::output::escape_human(&target.path.bytes()),
            target.outcome,
            target
                .detail
                .as_deref()
                .map(|d| format!(": {d}"))
                .unwrap_or_default()
        ));
        for path in &target.remaining {
            out.push_str(&format!(
                "  still here: {}\n",
                crate::output::escape_human(&path.bytes())
            ));
        }
    }
    for bin in &doc.recycle_bins {
        out.push_str(&format!(
            "Recycle Bin of {}: received {} item(s), {}{}\n",
            crate::output::escape_human(&bin.share.bytes()),
            bin.items,
            crate::output::human_bytes(bin.moved_bytes),
            if bin.admin_only {
                "; only administrators can restore them"
            } else {
                ""
            }
        ));
    }
    out.push_str(&format!("Note: {}\n", doc.space_release));
    out
}

fn save_result(path: &Path, doc: &ApplyDocument) -> Result<(), Failure> {
    let parent = path.parent().ok_or("no parent")?;
    let directory = guard::open_absolute_no_follow(parent)?;
    let mut file = sys::openat(
        directory.as_fd(),
        path.file_name().ok_or("no name")?,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0o600,
    )?;
    serde_json::to_writer_pretty(&mut file, doc)?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Marks every report of the filesystem out of date and the selected
/// subvolumes' cached units stale (SPEC 5.7 "Reports after cleanup").
fn invalidate(state_dir: &Path, uid: u32, plan: &Plan, result: &Path) -> Result<(), Failure> {
    let store = Store::open(state_dir, uid)?;
    let mark = crate::document::CleanupMark {
        plan_id: plan.plan_id.clone(),
        execution_log: result
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        applied_at: crate::time::rfc3339(SystemTime::now()),
    };
    store.record_cleanup(&plan.filesystem_uuid, mark)?;
    let mut trees: Vec<u64> = plan.targets.iter().filter_map(|t| t.tree).collect();
    trees.sort_unstable();
    trees.dedup();
    if !trees.is_empty() {
        cache::mark_stale(
            &store.root().join("cache"),
            &plan.filesystem_uuid,
            uid,
            &trees,
        )?;
    }
    Ok(())
}

/// What moved into one shared folder's Recycle Bin at one time, for the
/// Recycle Bin page's "moved by Reclaim" (SPEC 5.16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveRecord {
    #[serde(with = "crate::report::hex_bytes")]
    pub volume: Vec<u8>,
    #[serde(with = "crate::report::hex_bytes")]
    pub share: Vec<u8>,
    pub at: u64,
    pub items: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct MoveRecords {
    moves: Vec<MoveRecord>,
}

const MOVES_FILE: &str = "moved.json";
const MOVES_KEPT: usize = 256;

fn record_moves(
    paths: &CleanupStatePaths,
    plan: &Plan,
    results: &[moves::Moved],
) -> Result<(), Failure> {
    let file = paths
        .plans
        .parent()
        .ok_or("no cleanup dir")?
        .join(MOVES_FILE);
    let mut records = read_moves(&file);
    let at = guard::now_seconds();
    for (target, result) in plan.targets.iter().zip(results) {
        // Only what went into a Recycle Bin can be restored from it.
        if result.moved.entries == 0 || result.for_good {
            continue;
        }
        match records
            .moves
            .iter_mut()
            .find(|m| m.at == at && m.volume == plan.volume && m.share == target.share)
        {
            Some(record) => {
                record.items += 1;
                record.allocated_bytes += result.moved.allocated_bytes;
            }
            None => records.moves.push(MoveRecord {
                volume: plan.volume.clone(),
                share: target.share.clone(),
                at,
                items: 1,
                allocated_bytes: result.moved.allocated_bytes,
            }),
        }
    }
    let excess = records.moves.len().saturating_sub(MOVES_KEPT);
    records.moves.drain(..excess);
    let tmp = file.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    serde_json::to_writer(&mut out, &records)?;
    out.sync_all()?;
    std::fs::rename(&tmp, &file)?;
    Ok(())
}

fn read_moves(file: &Path) -> MoveRecords {
    guard::open_absolute_no_follow(file)
        .ok()
        .and_then(|f| guard::private_file(f, sys::effective_uid()).ok())
        .and_then(|f| serde_json::from_reader(std::io::BufReader::new(f)).ok())
        .unwrap_or_default()
}

/// The moves into shared-folder Recycle Bins of `volume` after `since`,
/// one line per shared folder.
pub fn moved_since(state_dir: &Path, volume: &[u8], since: u64) -> Vec<MoveRecord> {
    let records = read_moves(&state_dir.join("cleanup").join(MOVES_FILE));
    let mut out: Vec<MoveRecord> = Vec::new();
    for m in records
        .moves
        .into_iter()
        .filter(|m| m.volume == volume && m.at > since)
    {
        match out.iter_mut().find(|o| o.share == m.share) {
            Some(o) => {
                o.items += m.items;
                o.allocated_bytes += m.allocated_bytes;
                o.at = o.at.max(m.at);
            }
            None => out.push(m),
        }
    }
    out
}

/// A `cleanup apply` run as a job: progress after each run of DSM's tool
/// (at most every two seconds) and each item, and a cancel request that
/// stops before the next run.
struct ApplyJob {
    live: Live,
    started: Instant,
    warned: bool,
    calls: u64,
    saved: Option<Instant>,
    poll: Poll,
    cancel: bool,
    tally: Tally,
    /// The plan's allocated size, summed once.
    bytes_total: u64,
}

/// What a running move asks of the job between DSM's runs.
struct JobControl<'a> {
    job: &'a mut ApplyJob,
    plan: &'a Plan,
}

impl moves::MoveControl for JobControl<'_> {
    fn cancelled(&mut self) -> bool {
        sys::stop_signal().is_some() || self.job.cancel_requested()
    }

    fn stepped(&mut self) {
        self.job.calls += 1;
        let now = Instant::now();
        if self
            .job
            .saved
            .is_none_or(|saved| now.duration_since(saved) >= jobs::PROGRESS_INTERVAL)
        {
            self.job.progress(self.plan);
        }
    }
}

/// The outcomes so far, kept as each item ends: progress never counts
/// the results again.
#[derive(Debug, Default, Clone, Copy)]
struct Tally {
    done: u64,
    moved: u64,
    deleted: u64,
    already_absent: u64,
    partial: u64,
    changed: u64,
    failed: u64,
    bytes_moved: u64,
}

impl Tally {
    fn add(&mut self, result: &moves::Moved) {
        self.done += 1;
        self.bytes_moved += result.moved.allocated_bytes;
        *match result.outcome {
            Outcome::Moved => &mut self.moved,
            Outcome::Deleted => &mut self.deleted,
            Outcome::AlreadyAbsent => &mut self.already_absent,
            Outcome::Partial => &mut self.partial,
            Outcome::Changed => &mut self.changed,
            Outcome::Failed => &mut self.failed,
        } += 1;
    }
}

impl ApplyJob {
    fn start(
        jobs_dir: &Path,
        uid: u32,
        id: &str,
        plan: &Plan,
        profile: resource::Profile,
    ) -> Result<Self, Failure> {
        jobs::prune(jobs_dir, jobs::FINISHED_KEPT)?;
        let now = SystemTime::now();
        // A launcher that detached this apply queued its job first.
        let live = match jobs::load(jobs_dir, id) {
            Ok(queued) if queued.kind != jobs::JobKind::CleanupApply => {
                return Err(format!("job {id} is not a cleanup job").into());
            }
            Ok(_) => Live::launch(jobs_dir, uid, id, unix_seconds(now))?,
            Err(jobs::JobError::NotFound(_)) => {
                Live::start(jobs_dir, uid, cleanup_job(id, plan, profile, now))?
            }
            Err(error) => return Err(error.into()),
        };
        let now = Instant::now();
        let mut job = ApplyJob {
            live,
            started: now,
            warned: false,
            calls: 0,
            saved: None,
            poll: Poll::new(jobs::POLL_INTERVAL, now),
            cancel: false,
            tally: Tally::default(),
            bytes_total: plan.size().allocated_bytes,
        };
        job.progress(plan);
        Ok(job)
    }

    /// One more item ended; its progress is saved at most every
    /// [`jobs::PROGRESS_INTERVAL`], not after each of many small items.
    fn ended(&mut self, plan: &Plan, result: &moves::Moved) {
        self.tally.add(result);
        let now = Instant::now();
        if self
            .saved
            .is_none_or(|saved| now.duration_since(saved) >= jobs::PROGRESS_INTERVAL)
        {
            self.progress(plan);
        }
    }

    fn cancel_requested(&mut self) -> bool {
        if !self.cancel && self.poll.due(Instant::now()) {
            self.cancel = self.live.requested() == Some(Request::Cancel);
        }
        self.cancel
    }

    fn progress(&mut self, plan: &Plan) {
        self.saved = Some(Instant::now());
        let progress = progress_of(
            plan,
            &self.tally,
            self.bytes_total,
            self.calls,
            self.started.elapsed(),
        );
        self.live.job.cleanup = Some(progress);
        if let Err(error) = self.live.save_progress()
            && !self.warned
        {
            eprintln!("synoreclaim: warning: cleanup progress not updated: {error}");
            self.warned = true;
        }
    }

    fn finish(
        mut self,
        plan: &Plan,
        document: &ApplyDocument,
        exit_code: i32,
    ) -> Result<(), Failure> {
        // The last counts, whatever the interval left unsaved.
        self.progress(plan);
        let mut codes: Vec<String> = document
            .envelope
            .errors
            .iter()
            .map(|e| e.code.to_string())
            .collect();
        codes.sort();
        codes.dedup();
        let completion = document.envelope.completion;
        self.live.record_result(
            jobs::JobResult::new(completion.name(), exit_code, &[], codes),
            (serde_json::to_string_pretty(document)? + "\n").as_bytes(),
        )?;
        let (state, reason) = cleanup_job_state(completion, exit_code);
        self.live
            .finish(state, reason, unix_seconds(SystemTime::now()))?;
        Ok(())
    }
}

fn progress_of(
    plan: &Plan,
    tally: &Tally,
    bytes_total: u64,
    calls: u64,
    elapsed: Duration,
) -> crate::progress::CleanupProgress {
    crate::progress::CleanupProgress {
        plan_id: plan.plan_id.clone(),
        items_total: plan.targets.len() as u64,
        items_done: tally.done,
        moved: tally.moved,
        deleted: tally.deleted,
        already_absent: tally.already_absent,
        partial: tally.partial,
        changed: tally.changed,
        failed: tally.failed,
        bytes_total,
        bytes_moved: tally.bytes_moved,
        calls_done: calls,
        elapsed_s: elapsed.as_secs(),
        updated: crate::time::rfc3339(SystemTime::now()),
    }
}

/// The state a cleanup job ends in (SPEC 5.7).
fn cleanup_job_state(completion: Completion, exit_code: i32) -> (JobState, Option<&'static str>) {
    match completion {
        Completion::Complete => (JobState::Completed, None),
        Completion::Cancelled => (JobState::Cancelled, Some("cancelled")),
        Completion::Paused => (JobState::Partial, Some("signal")),
        Completion::Partial if exit_code != 1 => (JobState::Partial, None),
        _ => (JobState::Failed, Some("failed")),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn cleanup_delete(
    targets: TargetArgs,
    output_path: Option<PathBuf>,
    yes: bool,
    json: bool,
    quiet: bool,
    profile: resource::Profile,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let subject = subject(&targets);
    let tools = Tools::dsm();
    let plan = match make_plan(
        Selected {
            targets,
            expect: None,
            duplicates: None,
            staged: None,
        },
        output_path,
        state_dir,
        &tools,
    ) {
        Ok(plan) => plan,
        Err(error) => return finish_cleanup(json, "cleanup-delete", &subject, Err(error)),
    };
    if !quiet && !json {
        eprint!("{}", render_plan(&plan));
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(plan.plan_file.bytes()));
    // `cleanup delete` confirms only moves into a Recycle Bin.
    let result = apply_plan(
        "cleanup-delete",
        &path,
        (yes, false),
        json,
        profile,
        state_dir,
        &new_apply_attempt(),
        &tools,
    );
    finish_cleanup(json, "cleanup-delete", &subject, result)
}

#[cfg(test)]
mod tests {

    /// A folder right below the volume that DSM does not list (left over
    /// from a restore, say) is refused as outside every shared folder, not
    /// as a package or a shared folder: what it holds is not the reason.
    #[test]
    fn a_folder_below_the_volume_that_dsm_does_not_list_is_outside_every_share() {
        let shares: std::collections::HashSet<Vec<u8>> = [b"photos".to_vec(), b"homes".to_vec()]
            .into_iter()
            .collect();
        let message = outside_every_share(b"restore-test", &shares).unwrap();
        assert!(message.contains("not a DSM shared folder"), "{message}");
        assert_eq!(outside_every_share(b"photos", &shares), None);
        let error = TargetError::new(
            JsonPath::new(b"/volume1/restore-test"),
            "protected_target",
            message,
        )
        .protected("outside_share", None);
        assert_eq!(error.reason(), Some("outside_share"));
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["reason"], "outside_share");
        assert!(json.get("package").is_none());
    }
    use super::*;

    /// Items in a folder below the volume that is no shared folder are
    /// removed for good, so apply refuses them until that is confirmed;
    /// hidden and file-system folders there are never touched.
    #[test]
    fn removing_for_good_needs_its_own_confirmation() {
        let target = |share: &[u8], delete: bool| Target {
            path: [b"/volume1/".as_slice(), share, b"/package-meta.tar"].concat(),
            share: share.to_vec(),
            rpath: b"package-meta.tar".to_vec(),
            kind: Kind::File,
            device: 1,
            inode: 2,
            tree: None,
            size: Size::default(),
            symlinks: 0,
            split: Vec::new(),
            delete,
        };
        let plan = |targets| Plan {
            format: moves::PLAN_FORMAT,
            plan_id: "cleanup-1-2".into(),
            created_at: 100,
            expires_at: 200,
            volume: b"/volume1".to_vec(),
            filesystem_uuid: "f".into(),
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            targets,
            keep: Vec::new(),
            checksum: String::new(),
        };
        let moves_only = plan(vec![target(b"photos", false)]);
        assert!(deletes_confirmed(&moves_only, false).is_ok());
        let deletes = plan(vec![
            target(b"photos", false),
            target(b"restore-test", true),
        ]);
        let refused = deletes_confirmed(&deletes, false).unwrap_err();
        assert_eq!(refused.code, "delete_not_confirmed");
        assert!(refused.message.contains("1 item(s)"), "{}", refused.message);
        assert!(deletes_confirmed(&deletes, true).is_ok());
        for name in [&b".hidden"[..], b"lost+found"] {
            assert!(hidden_system_folder(name).is_some(), "{name:?}");
        }
        assert_eq!(hidden_system_folder(b"restore-test"), None);
    }

    fn moved(outcome: Outcome) -> moves::Moved {
        moves::Moved {
            outcome,
            moved: Size::default(),
            remaining: Size::default(),
            remaining_complete: true,
            remaining_paths: Vec::new(),
            cancelled: false,
            detail: None,
            for_good: false,
        }
    }

    #[test]
    fn apply_exits_by_what_moved() {
        assert_eq!(
            apply_status(&[moved(Outcome::Moved), moved(Outcome::AlreadyAbsent)], 2),
            0
        );
        assert_eq!(apply_status(&[moved(Outcome::Moved)], 2), 3);
        assert_eq!(apply_status(&[moved(Outcome::Partial)], 1), 3);
        assert_eq!(
            apply_status(&[moved(Outcome::Failed), moved(Outcome::Changed)], 2),
            4
        );
    }

    #[test]
    fn moves_since_a_scan_add_up_per_shared_folder() {
        let dir = std::env::temp_dir().join(format!("reclaim-moves-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cleanup")).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let records = MoveRecords {
            moves: vec![
                MoveRecord {
                    volume: b"/v".to_vec(),
                    share: b"s".to_vec(),
                    at: 10,
                    items: 1,
                    allocated_bytes: 4,
                },
                MoveRecord {
                    volume: b"/v".to_vec(),
                    share: b"s".to_vec(),
                    at: 20,
                    items: 2,
                    allocated_bytes: 8,
                },
                MoveRecord {
                    volume: b"/v".to_vec(),
                    share: b"t".to_vec(),
                    at: 30,
                    items: 1,
                    allocated_bytes: 1,
                },
                MoveRecord {
                    volume: b"/w".to_vec(),
                    share: b"s".to_vec(),
                    at: 30,
                    items: 1,
                    allocated_bytes: 1,
                },
            ],
        };
        let file = dir.join("cleanup").join(MOVES_FILE);
        std::fs::write(&file, serde_json::to_vec(&records).unwrap()).unwrap();
        std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .unwrap();
        let since = moved_since(&dir, b"/v", 15);
        assert_eq!(since.len(), 2);
        assert_eq!(
            (since[0].items, since[0].allocated_bytes, since[0].at),
            (2, 8, 20)
        );
        assert_eq!(since[1].share, b"t");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn plan(fsid: &str) -> Plan {
        Plan {
            format: moves::PLAN_FORMAT,
            plan_id: "cleanup-1-2".into(),
            created_at: 1,
            expires_at: 2,
            volume: b"/volume1".to_vec(),
            filesystem_uuid: fsid.into(),
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            targets: vec![Target {
                path: b"/volume1/s/a".to_vec(),
                share: b"s".to_vec(),
                rpath: b"a".to_vec(),
                kind: Kind::Directory,
                device: 1,
                inode: 300,
                tree: Some(257),
                size: Size::default(),
                symlinks: 0,
                split: Vec::new(),
                delete: false,
            }],
            keep: Vec::new(),
            checksum: String::new(),
        }
        .seal()
    }

    #[test]
    fn a_move_keeps_the_reports_of_its_filesystem_and_marks_them_stale() {
        use crate::document::Marked;
        let state = crate::spill::testing::scratch("cleanup-reports").join("state");
        let uid = crate::sys::effective_uid();
        let store = crate::state::Store::open(&state, uid).unwrap();
        let mut header = crate::report::testing::header(Vec::new());
        let plan = plan(&header.fsid.clone());
        header.fsid = plan.filesystem_uuid.clone();
        let tree = crate::model::tree::ScopeTree::new();
        store
            .publish(&header, |out| {
                crate::report::testing::write_footprint(out, &header, &tree)
            })
            .unwrap();
        let result = state.join("cleanup/logs/cleanup-1-2-3.json");
        invalidate(&state, uid, &plan, &result).unwrap();
        assert_eq!(store.reports(&header.fsid).unwrap().reports.len(), 1);
        let Marked::By(mark) = store.cleanup_marks(&header.fsid).of(&header.job_id) else {
            panic!("report not marked");
        };
        assert_eq!(mark.plan_id, plan.plan_id);
        assert_eq!(mark.execution_log, "cleanup-1-2-3.json");
    }

    #[test]
    fn a_move_drops_the_cached_units_of_the_subvolumes_it_moves_from() {
        use crate::cache::{Budget, Cache, Reuse};
        use crate::units::UnitKey;
        let state = crate::spill::testing::scratch("cleanup-cache");
        std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let uid = crate::sys::effective_uid();
        let plan = plan("00112233-4455-6677-8899-aabbccddeeff");
        let fsid = plan.filesystem_uuid.clone();
        let open = || {
            let reuse = Reuse {
                writable_max_age: 0,
                trust_writable: false,
            };
            let budget = Budget {
                max_bytes: u64::MAX,
                min_free: 0,
            };
            Cache::open(&state.join("cache"), &fsid, uid, reuse, budget).unwrap()
        };
        let key = |subvolume| UnitKey {
            subvolume,
            uuid: "00ff".into(),
            generation: 10,
            readonly: true,
            slot: 7,
            inputs: 9,
        };
        let mut cache = open();
        for subvolume in [257, 258] {
            let staging = state.join(format!("staging-{subvolume}"));
            std::fs::create_dir(&staging).unwrap();
            std::fs::write(staging.join("tree"), b"t").unwrap();
            std::fs::write(staging.join("extents"), b"e").unwrap();
            cache.seal(&key(subvolume), 1000, &staging).unwrap();
        }
        drop(cache);
        invalidate(&state, uid, &plan, Path::new("/s/cleanup/logs/p-1.json")).unwrap();
        let mut cache = open();
        assert!(
            cache.find(&key(257), 1001).is_none(),
            "moved-from subvolume"
        );
        assert!(cache.find(&key(258), 1001).is_some(), "untouched subvolume");
        std::fs::remove_dir_all(state).unwrap();
    }

    /// The documents the goldens in tests/golden/json hold, from the code
    /// itself: `SYNORECLAIM_PRINT_GOLDEN=1` prints them.
    fn golden_documents() -> (serde_json::Value, serde_json::Value) {
        let mut plan = plan("00112233-4455-6677-8899-aabbccddeeff");
        plan.targets[0].path = b"/volume1/photos/2019".to_vec();
        plan.targets[0].share = b"photos".to_vec();
        plan.targets[0].rpath = b"2019".to_vec();
        plan.targets[0].size = Size {
            entries: 3,
            files: 2,
            allocated_bytes: 2 << 20,
        };
        let mut second = plan.targets[0].clone();
        second.path = b"/volume1/photos/old.iso".to_vec();
        second.rpath = b"old.iso".to_vec();
        second.kind = Kind::File;
        second.size = Size {
            entries: 1,
            files: 1,
            allocated_bytes: 4 << 20,
        };
        plan.targets.push(second);
        let plan = plan.seal();
        let item = |t: &Target| PlanItem {
            path: JsonPath::new(&t.path),
            kind: t.kind,
            size: t.size,
            symlinks: 0,
            drive: false,
        };
        let mut envelope = Envelope::new("cleanup-plan", std::time::UNIX_EPOCH);
        envelope.warnings.push(Notice::new(
            "deletes_for_good",
            format!(
                "1 item(s) have no Recycle Bin (in no shared folder, or in a shared folder whose Recycle Bin is off): apply removes them for good, only with --confirm-delete; to move them to a Recycle Bin instead, turn it on ({}) and make a new plan",
                moves::ENABLE_ROUTE
            ),
        ));
        let document = PlanDocument {
            envelope,
            plan_id: plan.plan_id.clone(),
            plan_file: JsonPath::new(
                b"/var/packages/SynoReclaim/var/state/cleanup/plans/cleanup-1-2.json",
            ),
            expires_at: "2026-11-14T00:00:00Z".into(),
            items: plan.targets.len() as u64 + 1,
            deletes: 1,
            paths: plan
                .targets
                .iter()
                .map(|t| JsonPath::new(&t.path))
                .collect(),
            size: plan.size().plus(Size {
                entries: 1,
                files: 1,
                allocated_bytes: 1 << 20,
            }),
            shares: vec![
                PlanShare {
                    share: JsonPath::new(b"photos"),
                    path: JsonPath::new(b"/volume1/photos"),
                    recycle_bin: BinStatus {
                        enabled: true,
                        admin_only: true,
                    },
                    item_count: plan.targets.len() as u64,
                    drive_items: 0,
                    items: plan.targets.iter().map(item).collect(),
                    size: plan.size(),
                    symlinks: 0,
                    delete: false,
                    delete_reason: None,
                },
                PlanShare {
                    share: JsonPath::new(b"video"),
                    path: JsonPath::new(b"/volume1/video"),
                    recycle_bin: BinStatus {
                        enabled: false,
                        admin_only: false,
                    },
                    item_count: 1,
                    drive_items: 0,
                    items: vec![PlanItem {
                        path: JsonPath::new(b"/volume1/video/a.mkv"),
                        kind: Kind::File,
                        size: Size {
                            entries: 1,
                            files: 1,
                            allocated_bytes: 1 << 20,
                        },
                        symlinks: 0,
                        drive: false,
                    }],
                    size: Size {
                        entries: 1,
                        files: 1,
                        allocated_bytes: 1 << 20,
                    },
                    symlinks: 0,
                    delete: true,
                    delete_reason: Some(DeleteReason::RecycleBinOff),
                },
            ],
            duplicates: None,
        };
        let results = vec![
            moves::Moved {
                outcome: Outcome::Moved,
                moved: plan.targets[0].size,
                remaining: Size::default(),
                remaining_complete: true,
                remaining_paths: Vec::new(),
                cancelled: false,
                detail: None,
                for_good: false,
            },
            moves::Moved {
                outcome: Outcome::Partial,
                moved: Size {
                    entries: 0,
                    files: 0,
                    allocated_bytes: 0,
                },
                remaining: plan.targets[1].size,
                remaining_complete: true,
                remaining_paths: vec![plan.targets[1].path.clone()],
                cancelled: false,
                detail: Some("DSM's synorecycle exited 255".into()),
                for_good: false,
            },
        ];
        let mut applied = apply_document("cleanup-apply", &plan, &results, true);
        applied.envelope.generated_at = "1970-01-01T00:00:00Z".into();
        applied.envelope.completion = Completion::Partial;
        (
            serde_json::to_value(&document).unwrap(),
            serde_json::to_value(&applied).unwrap(),
        )
    }

    #[test]
    fn the_goldens_are_what_plan_and_apply_print() {
        let (plan, applied) = golden_documents();
        if std::env::var_os("SYNORECLAIM_PRINT_GOLDEN").is_some() {
            println!("PLAN {}", serde_json::to_string_pretty(&plan).unwrap());
            println!("APPLY {}", serde_json::to_string_pretty(&applied).unwrap());
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/json");
        let read = |name: &str| -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(root.join(name)).unwrap()).unwrap()
        };
        let keys = |v: &serde_json::Value| -> Vec<String> {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&plan), keys(&read("cleanup-plan.json")));
        assert_eq!(plan["shares"], read("cleanup-plan.json")["shares"]);
        assert_eq!(keys(&applied), keys(&read("cleanup-apply.json")));
        assert_eq!(applied["targets"], read("cleanup-apply.json")["targets"]);
        assert_eq!(applied["recycle_bins"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn allowed_roots_only_narrow_what_moves() {
        let mut config = guard::CleanupConfig::default();
        assert!(within_allowed_roots(&config, b"/volume1/s/a").is_ok());
        config.allowed_roots = Some(vec!["/volume1/s".into(), "relative".into()]);
        assert!(within_allowed_roots(&config, b"/volume1/s/a").is_ok());
        assert_eq!(
            within_allowed_roots(&config, b"/volume1/t/a")
                .unwrap_err()
                .code,
            "outside_allowed_roots"
        );
        assert!(within_allowed_roots(&config, b"/volume1/sa").is_err());
    }

    #[test]
    fn a_report_read_sizes_the_items_and_marks_the_large_folders_to_split() {
        let resolved = |path: &str| Resolved {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            requested: path.into(),
            path: path.into(),
            scope: Scope::Path,
            fsid: [0; 16],
            mount: "/volume1".into(),
            tree: 256,
            ino: 0,
        };
        let target = |path: &str, inode| Target {
            path: path.as_bytes().to_vec(),
            share: b"s".to_vec(),
            rpath: path.trim_start_matches("/volume1/s/").as_bytes().to_vec(),
            kind: Kind::Directory,
            device: 1,
            inode,
            tree: None,
            size: Size::default(),
            symlinks: 0,
            split: Vec::new(),
            delete: false,
        };
        let mut checked = vec![
            (resolved("/volume1/s/big"), target("/volume1/s/big", 300)),
            (
                resolved("/volume1/s/replaced"),
                target("/volume1/s/replaced", 400),
            ),
        ];
        let indices = [0, 1];
        let big = |entries: u64| Size {
            entries,
            files: entries - 1,
            allocated_bytes: entries * 4096,
        };
        let mut sizing = Sizing::new(&mut checked, &indices);
        sizing.visit(b"/volume1/s", 1, true, false, 256, big(400_010));
        sizing.visit(b"/volume1/s/big", 2, true, false, 300, big(300_005));
        sizing.visit(b"/volume1/s/big/a", 3, true, false, 301, big(200_000));
        sizing.visit(b"/volume1/s/big/a/x", 4, true, false, 302, big(150_000));
        sizing.visit(b"/volume1/s/big/a/y", 4, true, false, 303, big(50_000));
        sizing.visit(b"/volume1/s/big/b", 3, true, false, 304, big(100_000));
        // Out of the item again: a large sibling is not its to split.
        sizing.visit(b"/volume1/s/other", 2, true, false, 305, big(200_000));
        // The scan saw another inode at this path: it is walked instead.
        sizing.visit(b"/volume1/s/replaced", 2, true, false, 999, big(10));
        let sized = sizing.sized;
        assert_eq!(sized, [true, false]);
        assert_eq!(checked[0].1.size, big(300_005));
        assert_eq!(
            checked[0].1.split,
            [b"".to_vec(), b"a".to_vec(), b"a/x".to_vec()]
        );
        assert!(checked[1].1.split.is_empty());
        assert_eq!(checked[1].1.size, Size::default());
    }

    /// A gone copy of a selected duplicate group names the group, so Review
    /// can take the group out of the list.
    #[test]
    fn a_refused_duplicates_path_names_its_group() {
        let group_of: HashMap<Vec<u8>, String> = [
            (b"/volume1/media/Music".to_vec(), "a".repeat(32)),
            (b"/volume1/media/Inbox/x.mov".to_vec(), "b".repeat(32)),
        ]
        .into_iter()
        .collect();
        let mut errors = vec![
            TargetError::new(JsonPath::new(b"/volume1/media/Music"), "not_found", "gone"),
            TargetError::new(
                JsonPath::new(b"/volume1/media/Music/a.flac"),
                "target_changed",
                "changed",
            ),
            TargetError::new(
                JsonPath::new(b"/volume1/media/Inbox/x.mov"),
                "not_found",
                "gone",
            ),
            TargetError::new(
                JsonPath::new(b"/volume1/media/Musical"),
                "not_found",
                "gone",
            ),
        ];
        name_groups(&mut errors, &group_of);
        let groups: Vec<Option<&str>> = errors
            .iter()
            .map(|e| e.duplicate.as_deref().and_then(|d| d.group.as_deref()))
            .collect();
        let (a, b) = ("a".repeat(32), "b".repeat(32));
        assert_eq!(
            groups,
            [Some(a.as_str()), Some(a.as_str()), Some(b.as_str()), None]
        );
        let json = serde_json::to_value(&errors[0]).unwrap();
        assert_eq!(json["group"], a.as_str());
        assert!(
            serde_json::to_value(&errors[3])
                .unwrap()
                .get("group")
                .is_none()
        );
    }

    /// DSM's Recycle Bin drops symlinks, and folders holding only them, on a
    /// move, as File Station's delete does: the plan counts them from the
    /// report records it reads anyway, so Review can say how many.
    #[test]
    fn a_report_read_counts_the_symlinks_each_item_holds() {
        let resolved = |path: &str| Resolved {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            requested: path.into(),
            path: path.into(),
            scope: Scope::Path,
            fsid: [0; 16],
            mount: "/volume1".into(),
            tree: 256,
            ino: 0,
        };
        let target = |path: &str, kind, inode| Target {
            path: path.as_bytes().to_vec(),
            share: b"s".to_vec(),
            rpath: path.trim_start_matches("/volume1/s/").as_bytes().to_vec(),
            kind,
            device: 1,
            inode,
            tree: None,
            size: Size::default(),
            symlinks: 0,
            split: Vec::new(),
            delete: false,
        };
        let mut checked = vec![
            (
                resolved("/volume1/s/dtc"),
                target("/volume1/s/dtc", Kind::Directory, 300),
            ),
            (
                resolved("/volume1/s/link"),
                target("/volume1/s/link", Kind::File, 400),
            ),
            (
                resolved("/volume1/s/plain"),
                target("/volume1/s/plain", Kind::Directory, 500),
            ),
        ];
        let indices = [0, 1, 2];
        let one = Size {
            entries: 1,
            files: 0,
            allocated_bytes: 0,
        };
        let mut sizing = Sizing::new(&mut checked, &indices);
        sizing.visit(b"/volume1/s", 1, true, false, 256, one);
        sizing.visit(b"/volume1/s/dtc", 2, true, false, 300, one);
        sizing.visit(b"/volume1/s/dtc/a.c", 3, false, false, 301, one);
        sizing.visit(b"/volume1/s/dtc/arm", 3, false, true, 302, one);
        // A folder holding only symlinks: each one counts for the item.
        sizing.visit(b"/volume1/s/dtc/prefixes", 3, true, false, 303, one);
        sizing.visit(b"/volume1/s/dtc/prefixes/a", 4, false, true, 304, one);
        sizing.visit(b"/volume1/s/dtc/prefixes/b", 4, false, true, 305, one);
        // Out of the folder: a selected symlink counts itself.
        sizing.visit(b"/volume1/s/link", 2, false, true, 400, one);
        sizing.visit(b"/volume1/s/other-link", 2, false, true, 450, one);
        sizing.visit(b"/volume1/s/plain", 2, true, false, 500, one);
        sizing.visit(b"/volume1/s/plain/f", 3, false, false, 501, one);
        let symlinks: Vec<u64> = checked.iter().map(|(_, t)| t.symlinks).collect();
        assert_eq!(symlinks, [3, 1, 0]);
    }

    /// `n` empty files in shared folder `s` of a scratch volume, 1,000 a
    /// folder, a plan moving each of them, and fake DSM tools that move in
    /// this process.
    fn many_files(label: &str, n: usize) -> (PathBuf, PathBuf, Plan, Tools) {
        let base =
            std::env::temp_dir().join(format!("reclaim-many-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let volume = base.join("volume1");
        let share = volume.join("s");
        let tools_dir = base.join("tools");
        std::fs::create_dir_all(&tools_dir).unwrap();
        let mut targets = Vec::with_capacity(n);
        for i in 0..n {
            let dir = share.join(format!("d{:03}", i / 1000));
            if i % 1000 == 0 {
                std::fs::create_dir_all(&dir).unwrap();
            }
            let file = dir.join(format!("f{i:05}.txt"));
            std::fs::write(&file, b"").unwrap();
            targets.push(moves::tests::planned(&file, &share));
        }
        let mut tools = moves::tests::fake_tools(&tools_dir, &share, "yes", true);
        tools.in_process = Some(share.clone());
        let now = SystemTime::now();
        let plan = Plan {
            format: moves::PLAN_FORMAT,
            plan_id: guard::new_plan_id(now, 1),
            created_at: unix_seconds(now),
            expires_at: unix_seconds(now) + moves::PLAN_LIFETIME_SECONDS,
            volume: volume.as_os_str().as_bytes().to_vec(),
            filesystem_uuid: "00112233-4455-6677-8899-aabbccddeeff".into(),
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            targets,
            keep: Vec::new(),
            checksum: String::new(),
        }
        .seal();
        (base, share, plan, tools)
    }

    fn files_below(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .flatten()
                .map(|e| {
                    if e.file_type().unwrap().is_dir() {
                        files_below(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
    }

    #[test]
    fn fifty_thousand_items_move_one_by_one_with_their_progress() {
        const N: usize = 50_000;
        let (base, share, plan, tools) = many_files("all", N);
        // The plan file stays small: about 60 bytes an item.
        let saved = serde_json::to_vec(&plan).unwrap();
        assert!(saved.len() < N * 80, "{} bytes", saved.len());
        assert_eq!(serde_json::from_slice::<Plan>(&saved).unwrap(), plan);
        // A plan of any size holds a lock per shared folder, not per item.
        assert_eq!(locked_paths(&plan), std::slice::from_ref(&share));
        let jobs = base.join("state").join("jobs");
        let uid = sys::effective_uid();
        let id = new_apply_attempt();
        let mut job = ApplyJob::start(&jobs, uid, &id, &plan, resource::Profile::Low).unwrap();
        let started = Instant::now();
        let moves = move_all(&tools, &plan, &mut job);
        assert!(!moves.cancelled && moves.stopped.is_none());
        assert_eq!(moves.results.len(), N);
        assert!(moves.results.iter().all(|r| r.outcome == Outcome::Moved));
        assert_eq!(job.tally.moved, N as u64);
        assert_eq!(files_below(&share.join("#recycle")), N);
        assert_eq!(files_below(&share) - N, 0, "nothing stays in place");
        let doc = apply_document("cleanup-apply", &plan, &moves.results, false);
        assert_eq!((doc.items, doc.moved), (N as u64, N as u64));
        assert!(
            doc.targets.is_empty(),
            "the result lists only what did not move"
        );
        assert!(serde_json::to_vec(&doc).unwrap().len() < 4096);
        job.finish(&plan, &doc, 0).unwrap();
        let ended = jobs::load(&jobs, &id).unwrap();
        let progress = ended.cleanup.clone().unwrap();
        assert_eq!(
            (progress.items_done, progress.items_total, progress.moved),
            (N as u64, N as u64, N as u64)
        );
        let manifest = serde_json::to_value(&ended).unwrap();
        assert_eq!(
            manifest["operands"].as_array().unwrap().len(),
            1,
            "the job names its shared folder, not each item"
        );
        eprintln!("moved {N} items in {:?}", started.elapsed());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn a_cancel_stops_a_large_apply_between_items_and_leaves_the_rest() {
        const N: usize = 20_000;
        let (base, share, plan, tools) = many_files("cancel", N);
        let jobs = base.join("state").join("jobs");
        let uid = sys::effective_uid();
        let id = new_apply_attempt();
        let mut job = ApplyJob::start(&jobs, uid, &id, &plan, resource::Profile::Low).unwrap();
        // Check for a cancel before every item, so the test needs no timing.
        job.poll = Poll::new(Duration::ZERO, Instant::now());
        let recycled = share.join("#recycle");
        let moves = std::thread::scope(|scope| {
            scope.spawn(|| {
                while files_below(&recycled) < 1000 {
                    std::thread::sleep(Duration::from_millis(5));
                }
                jobs::request(&jobs, uid, &id, Request::Cancel, 0).unwrap();
            });
            move_all(&tools, &plan, &mut job)
        });
        assert!(moves.cancelled);
        let done = moves.results.len();
        assert!(
            (1000..N).contains(&done),
            "{done} items moved before the cancel"
        );
        assert_eq!(
            files_below(&recycled),
            done,
            "each result is one item moved"
        );
        assert_eq!(
            files_below(&share) - done,
            N - done,
            "the rest stays in place"
        );
        assert_eq!(job.tally.done, done as u64);
        std::fs::remove_dir_all(base).unwrap();
    }

    /// A base folder whose every ancestor only root or this user can write,
    /// as cleanup state requires (`guard::check_state_ancestors`). The temp
    /// folder comes first; a container whose temp folder sits in a tree
    /// owned by the host user falls back to `$HOME` (root's own home there).
    fn trusted_test_base() -> Result<PathBuf, String> {
        let uid = sys::effective_uid();
        let mut tried = Vec::new();
        let candidates = [
            Some(std::env::temp_dir()),
            std::env::var_os("HOME").map(PathBuf::from),
        ];
        for base in candidates.into_iter().flatten() {
            match guard::check_state_ancestors(&base, uid) {
                Ok(()) => return Ok(base),
                Err(error) => tried.push(format!("{}: {error}", base.display())),
            }
        }
        Err(tried.join("; "))
    }

    #[test]
    fn the_test_base_for_cleanup_state_is_trusted_or_says_why_not() {
        match trusted_test_base() {
            Ok(base) => assert!(guard::check_state_ancestors(&base, sys::effective_uid()).is_ok()),
            Err(why) => assert!(!why.is_empty()),
        }
    }

    #[test]
    fn a_selection_of_fifty_thousand_paths_is_staged_in_chunks_and_used_once() {
        let base = match trusted_test_base() {
            Ok(base) => base,
            Err(why) => {
                // Cleanup state refuses a folder another user can write; with
                // no such folder here, the staging cannot run at all.
                eprintln!("skipped: no folder this user can trust for cleanup state ({why})");
                return;
            }
        };
        let state = base.join(format!("reclaim-select-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        std::fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let mut id: Option<String> = None;
        for chunk in 0..100 {
            let paths: Vec<serde_json::Value> = (0..500)
                .map(|i| {
                    let n = chunk * 500 + i;
                    serde_json::json!({
                        "path": crate::report::hex_bytes::encode(format!("/volume1/s/f{n:05}").as_bytes()),
                        "objectid": n,
                    })
                })
                .collect();
            let groups: Vec<String> = if chunk == 0 {
                vec!["a".repeat(32)]
            } else {
                Vec::new()
            };
            let text = serde_json::json!({"paths": paths, "groups": groups}).to_string();
            assert_eq!(stage_chunk(id.clone(), &text, true, &state).unwrap(), 0);
            if id.is_none() {
                let dir = state.join("cleanup").join("selections");
                let name = std::fs::read_dir(&dir)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .file_name();
                id = Some(name.to_string_lossy().into_owned());
            }
        }
        let id = id.unwrap();
        assert!(valid_selection_id(&id));
        let staged = take_selection(&state, &id).unwrap();
        assert_eq!(staged.paths.len(), 50_000);
        assert_eq!(staged.paths[49_999], b"/volume1/s/f49999");
        assert_eq!(staged.expected.len(), 50_000);
        assert_eq!(staged.expected[7].expect.objectid, 7);
        assert_eq!(staged.groups, ["a".repeat(32)]);
        // A plan uses the selection up; the review stages it again to recheck.
        assert_eq!(
            take_selection(&state, &id).unwrap_err().to_string(),
            "the staged selection is gone: select again"
        );
        for bad in ["../x", "selection-", "selection-zz", "plans"] {
            assert!(!valid_selection_id(bad), "{bad}");
            assert!(stage_chunk(Some(bad.into()), r#"{"paths":[]}"#, true, &state).is_err());
        }
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_report_read_sizes_fifty_thousand_items_in_one_pass() {
        let resolved = |path: &str| Resolved {
            filesystem: crate::filesystem::FilesystemKind::Btrfs,
            requested: path.into(),
            path: path.into(),
            scope: Scope::Path,
            fsid: [0; 16],
            mount: "/volume1".into(),
            tree: 256,
            ino: 0,
        };
        let mut checked: Vec<(Resolved, Target)> = (0..50_000u64)
            .map(|i| {
                let path = format!("/volume1/s/f{i:05}");
                let target = Target {
                    path: path.as_bytes().to_vec(),
                    share: b"s".to_vec(),
                    rpath: path.as_bytes()["/volume1/s/".len()..].to_vec(),
                    kind: Kind::File,
                    device: 1,
                    inode: 1000 + i,
                    tree: None,
                    size: Size::default(),
                    symlinks: 0,
                    split: Vec::new(),
                    delete: false,
                };
                (resolved(&path), target)
            })
            .collect();
        let indices: Vec<usize> = (0..checked.len()).collect();
        let started = Instant::now();
        let mut sizing = Sizing::new(&mut checked, &indices);
        sizing.visit(b"/volume1/s", 1, true, false, 256, Size::default());
        for i in 0..50_000u64 {
            let size = Size {
                entries: 1,
                files: 1,
                allocated_bytes: 4096,
            };
            sizing.visit(
                format!("/volume1/s/f{i:05}").as_bytes(),
                2,
                false,
                false,
                1000 + i,
                size,
            );
        }
        assert!(sizing.sized.iter().all(|s| *s));
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(checked[49_999].1.size.allocated_bytes, 4096);
    }
}
