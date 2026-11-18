//! `synoreclaim duplicates …`: the analysis job and the answers the
//! WebAPI gives from it (docs/duplicates.md "Commands").

use super::analysis::{self, Inputs, ReportFile};
use super::candidates;
use super::csum::Layout;
use super::model::{
    self, Amount, Analysis, Category, Cluster, Decisions, Member, Reason, Role, Settings, Tally,
    Unchecked, View,
};
use super::stamps;
use crate::btrfs::live::{LiveSource, OpenTarget};
use crate::btrfs::walk::{BatchBoundary, StopReason};
use crate::document::{Completion, Envelope};
use crate::output::{JsonPath, TargetError};
use crate::report::Header;
use crate::resolve::{Environment, fsid_text};
use crate::state::{self, Store};
use crate::target::Scope;
use crate::time::{rfc3339, unix_seconds};
use serde::Serialize;
use std::fs;
use std::io;
use std::ops::ControlFlow;
use std::os::fd::AsFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Failure = Box<dyn std::error::Error>;

/// Candidates one analysis reads at most (32 bytes each in memory).
pub const MAX_CANDIDATES: usize = 4_000_000;
/// Rows a page holds at most.
pub const MAX_PAGE: u32 = 200;
/// In-scope copies a row lists; the rest are counted.
const ROW_COPIES: usize = 5;
/// Preferred shares a volume keeps at most.
pub const MAX_PREFERRED: usize = 64;

/// The analysis results beside `report`.
pub fn results_path(report: &Path) -> PathBuf {
    report.with_extension("srdup")
}

fn settings_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("duplicates")
}

fn settings_name(fsid: &str) -> String {
    format!("{fsid}.json")
}

/// The keep choices and preferred shares of filesystem `fsid`.
pub fn load_settings(state_dir: &Path, fsid: &str) -> io::Result<Settings> {
    match fs::read(settings_dir(state_dir).join(settings_name(fsid))) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e),
    }
}

fn save_settings(state_dir: &Path, fsid: &str, settings: &Settings) -> Result<(), Failure> {
    let dir = settings_dir(state_dir);
    state::private_dir(&dir, crate::sys::effective_uid())?;
    state::replace_file(&dir, &settings_name(fsid), |out| {
        serde_json::to_writer(&mut *out, settings).map_err(io::Error::other)
    })?;
    Ok(())
}

fn file_name(path: &Path) -> Result<(&Path, &str), Failure> {
    let dir = path.parent().ok_or("report has no directory")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("report name is not UTF-8")?;
    Ok((dir, name))
}

/// Saves the results beside `report`. Stamps still held by folder
/// members move to the `.srstamps` file first, which the results then
/// point at: a page reads only the small results.
fn save_results(report: &Path, analysis: &mut Analysis) -> Result<(), Failure> {
    if stamps::inline(analysis) {
        let path = stamps::path_of(report);
        let (dir, name) = file_name(&path)?;
        state::replace_file(dir, name, |out| stamps::write(analysis, out))?;
    }
    let path = results_path(report);
    let (dir, name) = file_name(&path)?;
    state::replace_file(dir, name, |out| {
        serde_json::to_writer(&mut *out, &*analysis).map_err(io::Error::other)
    })?;
    Ok(())
}

/// The results file as it is: results of an older format with the
/// stamps inline read as they are.
fn read_results(report: &Path) -> Option<Analysis> {
    let bytes = fs::read(results_path(report)).ok()?;
    let mut a = serde_json::from_slice::<Analysis>(&bytes).ok()?;
    if a.tally_rule != model::TALLY_RULE {
        // Tallies of an older rule: they are computed when asked for.
        a.tallies.clear();
        a.tallies_complete = false;
    }
    match a.format {
        model::FORMAT => Some(a),
        model::FORMAT_INLINE_STAMPS => {
            a.format = model::FORMAT;
            Some(a)
        }
        _ => None,
    }
}

/// The results beside `report`. Results with the stamps inline are split
/// on first read; if that fails they are read whole again and kept as
/// they are, stamps inline.
fn load_results(report: &Path) -> Option<Analysis> {
    let mut a = read_results(report)?;
    if stamps::inline(&a) && save_results(report, &mut a).is_err() {
        return read_results(report);
    }
    Some(a)
}

/// Puts back the stamps of the folder groups in `ids`, which only
/// `cleanup.plan` needs. Stamps that are missing, of other results or
/// damaged refuse: a cleanup never runs without them.
fn attach_stamps(report: Option<&Path>, a: &mut Analysis, ids: &[String]) -> Result<(), Failure> {
    let ids: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    let wanted: Vec<usize> = (0..a.folders.len())
        .filter(|&i| a.folders[i].stamps.is_some() && ids.contains(a.folders[i].id.as_str()))
        .collect();
    if wanted.is_empty() {
        return Ok(());
    }
    let fail = |e: io::Error| {
        refusal(
            "duplicates_stale",
            format!(
                "the saved duplicate results are incomplete ({e}); run the duplicate analysis again"
            ),
        )
    };
    let report = report.ok_or_else(|| fail(io::Error::other("no report")))?;
    let mut file = stamps::Stamps::open(report, &a.stamps_id).map_err(fail)?;
    for i in wanted {
        file.attach(&mut a.folders[i]).map_err(fail)?;
    }
    Ok(())
}

/// The volume a `duplicates.analyze` job's params name.
fn analyzed_volume(params: &str, state_dir: &Path) -> Option<Volume> {
    let v: serde_json::Value = serde_json::from_str(params).ok()?;
    let path = PathBuf::from(v["volume"].as_str()?);
    volume(&path, state_dir).ok()
}

/// When an analysis is asked for: replaces the volume's settings file
/// unchanged, so the cached duplicates answers of the volume, which say
/// nothing runs, are not served again. Best effort.
pub fn invalidate_answers(state_dir: &Path, params: &str) {
    let Some(v) = analyzed_volume(params, state_dir) else {
        return;
    };
    if let Ok(settings) = load_settings(state_dir, &v.fsid) {
        let _ = save_settings(state_dir, &v.fsid, &settings);
    }
}

/// After an analysis, caches the answers the Duplicates page asks for
/// first, as a scan does for Explore: the volume's,
/// then each shared folder's page. Best effort: a missing answer is
/// computed into the cache when asked.
pub fn prewarm_answers(state_dir: &Path, params: &str) {
    let Some(v) = analyzed_volume(params, state_dir) else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let uid = crate::sys::effective_uid();
    let mount = Path::new(std::ffi::OsStr::from_bytes(&v.mount));
    let compute = |share: Option<&Path>| {
        let argv = crate::webapi::duplicates_prewarmed(state_dir, mount, share)?;
        let now = unix_seconds(SystemTime::now());
        crate::ui_cache::compute(state_dir, uid, &exe, &v.fsid, &v.mount, &argv, now)
            .ok()
            .flatten()
    };
    let Some((_, document, _)) = compute(None) else {
        return;
    };
    // The volume's children are its shared folders with duplicates.
    let doc: serde_json::Value = serde_json::from_str(&document).unwrap_or_default();
    let shares: Vec<PathBuf> = doc["children"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| serde_json::from_value::<JsonPath>(c["path"].clone()).ok())
        .map(|p| PathBuf::from(std::ffi::OsString::from_vec(p.bytes())))
        .collect();
    for share in shares {
        compute(Some(&share));
    }
}

/// A Btrfs volume by its mount and the newest report covering it whole.
struct Volume {
    mount: Vec<u8>,
    fsid: String,
    report: Option<(PathBuf, Header)>,
}

fn volume(path: &Path, state_dir: &Path) -> Result<Volume, Failure> {
    let env = Environment::load()?;
    let found = env
        .volumes
        .iter()
        .find(|v| v.primary.mount_point == path)
        .ok_or_else(|| format!("{} is not a volume mount", path.display()))?;
    if found.primary.fstype != "btrfs" {
        return Err("duplicates need a Btrfs volume".into());
    }
    let fsid = fsid_text(found.fsid.as_ref().map_err(|e| e.clone())?);
    let mount = path.as_os_str().as_bytes().to_vec();
    let store = Store::open(state_dir, crate::sys::effective_uid())?;
    let report = store
        .display_reports(&fsid)?
        .reports
        .into_iter()
        .find(|(_, h)| {
            h.mount == mount
                && h.roots
                    .iter()
                    .any(|r| r.scope == Scope::Volume && r.path == mount)
        });
    Ok(Volume {
        mount,
        fsid,
        report,
    })
}

/// Stops the analysis on SIGTERM or a job cancel.
struct Stop(&'static AtomicBool);

impl BatchBoundary for Stop {
    fn after_batch(&mut self) -> ControlFlow<StopReason> {
        if self.0.load(Ordering::Relaxed) {
            ControlFlow::Break(StopReason::Cancelled)
        } else {
            ControlFlow::Continue(())
        }
    }
}

#[derive(Serialize)]
struct AnalyzeDocument {
    #[serde(flatten)]
    envelope: Envelope,
    volume: JsonPath,
    report_job_id: Option<String>,
    groups: usize,
    folders: usize,
    totals: Option<TallyOut>,
    unchecked: Option<Unchecked>,
    stats: Option<Stats>,
}

#[derive(Serialize)]
struct Stats {
    candidates: u64,
    candidates_complete: bool,
    same_size: u64,
    excluded: u64,
    changed: u64,
    inode_searches: u64,
    checksum_searches: u64,
    reference_searches: u64,
    elapsed_ms: u64,
}

/// `duplicates analyze VOLUME`: the analysis job.
pub fn analyze(volume_path: &Path, json: bool, state_dir: &Path) -> Result<i32, Failure> {
    let started = SystemTime::now();
    let mut doc = AnalyzeDocument {
        envelope: Envelope::new("duplicates-analyze", started),
        volume: JsonPath::new(volume_path.as_os_str().as_bytes()),
        report_job_id: None,
        groups: 0,
        folders: 0,
        totals: None,
        unchecked: None,
        stats: None,
    };
    let result = (|| -> Result<Analysis, String> {
        let v = volume(volume_path, state_dir).map_err(|e| e.to_string())?;
        let (report, header) = v
            .report
            .ok_or("no volume report: run a scan of the whole volume first")?;
        let (list, complete) = candidates::read(&report, MAX_CANDIDATES)
            .map_err(|e| format!("the report has no duplicate candidates ({e}): run a new scan"))?;
        let target = OpenTarget::open(volume_path).map_err(|e| e.to_string())?;
        let info = crate::sys::fs_info(target.file.as_fd()).map_err(|e| e.to_string())?;
        let csum_size = crate::sys::fs_csum_size(target.file.as_fd())
            .ok()
            .flatten()
            .unwrap_or(4);
        let layout = Layout::new(info.sectorsize, info.nodesize, usize::from(csum_size));
        let mut source = LiveSource::new(target.file, 1 << 20);
        let stop = crate::sys::install_stop_handlers().map_err(|e| e.to_string())?;
        let mut boundary = Stop(stop);
        let subvolumes = crate::btrfs::subvolume::discover(&mut source, &mut boundary)
            .map_err(|e| e.to_string())?;
        let mut analysis = analysis::analyze(
            &mut source,
            &mut ReportFile(report.clone()),
            Inputs {
                mount: &v.mount,
                candidates: list,
                candidates_truncated: !complete,
                subvolumes: &subvolumes,
                layout,
                uncertain: header
                    .coverage
                    .namespace_uncertain_subvolumes
                    .as_ref()
                    .map(|trees| trees.iter().copied().collect()),
            },
            &mut boundary,
        )
        .map_err(|e| e.to_string())?;
        analysis.report_job_id = header.job_id.clone();
        analysis.mount = v.mount.clone();
        analysis.analyzed_at = unix_seconds(SystemTime::now());
        let settings = load_settings(state_dir, &v.fsid).map_err(|e| e.to_string())?;
        analysis.retally(&settings);
        save_results(&report, &mut analysis).map_err(|e| e.to_string())?;
        Ok(analysis)
    })();
    match result {
        Ok(a) => {
            doc.report_job_id = Some(a.report_job_id.clone());
            doc.groups = a.groups.iter().filter(|g| model::listed(g)).count();
            doc.folders = a.folders.iter().filter(|f| f.listed()).count();
            doc.totals = Some(TallyOut::of(&a.tally(&a.mount, &Settings::default())));
            doc.unchecked = Some(a.unchecked.clone());
            doc.stats = Some(Stats {
                candidates: a.candidates,
                candidates_complete: a.complete,
                same_size: a.same_size,
                excluded: a.excluded,
                changed: a.changed,
                inode_searches: a.searches,
                checksum_searches: a.csum_searches,
                reference_searches: a.reference_searches,
                elapsed_ms: started.elapsed().unwrap_or(Duration::ZERO).as_millis() as u64,
            });
            if !a.complete {
                doc.envelope.completion = Completion::Partial;
            }
        }
        Err(message) => fail(&mut doc.envelope, volume_path, message),
    }
    print(json, &doc, || {
        format!(
            "{} duplicate file groups, {} identical folders\n",
            doc.groups, doc.folders
        )
    })
}

fn fail(envelope: &mut Envelope, path: &Path, message: String) {
    envelope.completion = Completion::Failed;
    envelope.errors.push(TargetError::new(
        JsonPath::new(path.as_os_str().as_bytes()),
        "duplicates_unavailable",
        message,
    ));
}

fn print<T: Serialize>(json: bool, doc: &T, text: impl FnOnce() -> String) -> Result<i32, Failure> {
    let value = serde_json::to_value(doc)?;
    if json {
        println!("{value}");
    } else {
        print!("{}", text());
    }
    let code = match value["completion"].as_str() {
        Some("complete") => 0,
        Some("partial") => 3,
        _ => 1,
    };
    Ok(code)
}

/// Amounts as the API shows them: never negative.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AmountOut {
    pub copies: u64,
    pub can_free: u64,
    pub frees_later: u64,
}

impl AmountOut {
    fn of(a: Amount) -> Self {
        AmountOut {
            copies: a.copies.max(0) as u64,
            can_free: a.can_free.max(0) as u64,
            frees_later: a.frees_later.max(0) as u64,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct TallyOut {
    /// Listed file groups of every category plus identical folders.
    pub all: AmountOut,
    /// Listed file groups only.
    pub files: AmountOut,
    pub photos: AmountOut,
    pub videos: AmountOut,
    pub music: AmountOut,
    pub folders: AmountOut,
}

impl TallyOut {
    fn of(t: &Tally) -> Self {
        TallyOut {
            all: AmountOut::of(t.all()),
            files: AmountOut::of(t.files(None)),
            photos: AmountOut::of(t.photos),
            videos: AmountOut::of(t.videos),
            music: AmountOut::of(t.music),
            folders: AmountOut::of(t.folders),
        }
    }
}

/// Where the analysis of a volume stands.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisState {
    /// `none`: never analysed; `stale`: a newer scan exists; `ready`.
    pub state: &'static str,
    /// An analysis job is queued or running.
    pub running: bool,
    pub job_id: Option<String>,
    pub analyzed_at: Option<String>,
    pub report_job_id: Option<String>,
    /// Every candidate was read.
    pub complete: bool,
}

struct Loaded {
    volume: Volume,
    analysis: Option<Analysis>,
    /// The report whose results `analysis` is.
    results: Option<PathBuf>,
    settings: Settings,
    state: AnalysisState,
}

fn load(volume_path: &Path, state_dir: &Path) -> Result<Loaded, Failure> {
    let volume = volume(volume_path, state_dir)?;
    let found = volume.report.as_ref().and_then(|(report, header)| {
        // The newest report's own results, else an older report's.
        load_results(report)
            .map(|a| (report.clone(), a))
            .or_else(|| {
                let store = Store::open(state_dir, crate::sys::effective_uid()).ok()?;
                store
                    .display_reports(&header.fsid)
                    .ok()?
                    .reports
                    .iter()
                    .filter(|(_, h)| h.mount == volume.mount)
                    .find_map(|(p, _)| load_results(p).map(|a| (p.clone(), a)))
            })
    });
    let (results, analysis) = found.map_or((None, None), |(p, a)| (Some(p), Some(a)));
    let settings = load_settings(state_dir, &volume.fsid)?;
    let running = running_job(state_dir, &volume.mount);
    let newest = volume.report.as_ref().map(|(_, h)| h.job_id.clone());
    let state = AnalysisState {
        state: match &analysis {
            None => "none",
            Some(a) if Some(&a.report_job_id) != newest.as_ref() => "stale",
            Some(_) => "ready",
        },
        running: running.is_some(),
        job_id: running,
        analyzed_at: analysis
            .as_ref()
            .map(|a| rfc3339(UNIX_EPOCH + Duration::from_secs(a.analyzed_at))),
        report_job_id: analysis.as_ref().map(|a| a.report_job_id.clone()),
        complete: analysis.as_ref().is_some_and(|a| a.complete),
    };
    Ok(Loaded {
        volume,
        analysis,
        results,
        settings,
        state,
    })
}

/// A queued or running analysis job of the volume at `mount`.
fn running_job(state_dir: &Path, mount: &[u8]) -> Option<String> {
    let jobs = crate::jobs::list(&state_dir.join("jobs")).ok()?;
    jobs.into_iter()
        .filter(|j| j.kind == crate::jobs::JobKind::DuplicatesAnalyze && !j.state.is_finished())
        .find(|j| {
            j.planning.as_ref().is_some_and(|p| {
                serde_json::from_str::<serde_json::Value>(&p.params)
                    .ok()
                    .and_then(|v| v["volume"].as_str().map(|s| s.as_bytes() == mount))
                    .unwrap_or(false)
            })
        })
        .map(|j| j.job_id)
}

/// Shown when the preferred shares changed since the tallies were made:
/// the numbers are the old ones until the analysis runs again.
fn recalculating(loaded: &Loaded) -> bool {
    loaded.analysis.as_ref().is_some_and(|a| {
        loaded.state.running || a.preferred_shares != loaded.settings.preferred_shares
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CopyOut {
    pub path: JsonPath,
    pub share: String,
    pub mtime: String,
    pub role: Role,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeeperOut {
    pub path: JsonPath,
    pub share: String,
    pub mtime: String,
    pub reason: Reason,
    pub in_scope: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupInfo {
    pub id: String,
    /// `file`, `folder` or `folder_partial`.
    pub kind: &'static str,
    pub name: JsonPath,
    /// A file's size; a folder's bytes; the identical items' bytes of
    /// partly identical folders.
    pub size: u64,
    /// A folder's file count (the identical items' files).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<u64>,
    pub copies_total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Partly identical folders: how many identical items the row
    /// removes from each folder it deletes from (`duplicates items`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_items: Option<usize>,
}

/// Partly identical folders as a scope sees them: the first folder the
/// row deletes from, and the folder it is compared with.
#[derive(Debug, Clone, Serialize)]
pub struct PartialOut {
    /// The folder the row deletes from (the first, by share order).
    pub path: JsonPath,
    /// Its entries, and those of them that are identical items.
    pub same_items: u64,
    pub total_items: u64,
    pub same_files: u64,
    pub total_files: u64,
    pub same_bytes: u64,
    pub total_bytes: u64,
    /// The kept folder it is compared with.
    pub counterpart: JsonPath,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShareCount {
    pub share: String,
    pub count: u64,
}

/// One group as a scope sees it (the v5 row).
#[derive(Debug, Clone, Serialize)]
pub struct GroupView {
    pub group: GroupInfo,
    pub keeper: KeeperOut,
    /// In-scope copies other than the keeper, at most five.
    pub in_scope: Vec<CopyOut>,
    /// All in-scope copies other than the keeper.
    pub in_scope_total: usize,
    /// The copies this cleanup removes: none when the user kept every
    /// copy here (the row stays listed so the choice can be undone).
    pub removes: usize,
    pub elsewhere_by_share: Vec<ShareCount>,
    /// The shares of every copy this cleanup removes, by name: what the
    /// page checks against each share's Recycle Bin before selecting.
    pub remove_shares: Vec<String>,
    /// Copies right below the volume (a shared folder itself, or outside
    /// every one): they stay whatever is chosen.
    pub unmovable: Vec<JsonPath>,
    /// Copies inside copies of a listed identical folder: that folder
    /// group removes and counts them, never this row.
    pub in_identical_folders: u64,
    pub can_free: u64,
    pub frees_later: u64,
    /// A copy this cleanup removes lies in Synology Drive.
    pub touches_drive: bool,
    /// Every copy shares one set of extents (clones): deleting frees
    /// nothing.
    pub shared: bool,
    /// Partly identical folders only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial: Option<PartialOut>,
}

fn time(seconds: u64) -> String {
    rfc3339(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// One listed group of either kind, ready for a view.
struct Row<'a> {
    info: GroupInfo,
    members: Vec<Member<'a>>,
    clusters: Vec<Cluster>,
    drive: Vec<bool>,
    /// A folder group's own record.
    folder: Option<&'a model::FolderGroup>,
    /// What a cleanup of the group keeps and removes on its whole volume
    /// ([`model::Decisions`]): a scope never changes it.
    view: View,
}

fn rows<'a>(d: &mut Decisions<'a>, list: List, category: Option<Category>) -> Vec<Row<'a>> {
    let analysis = d.analysis();
    match list {
        List::Files => (0..analysis.groups.len())
            .filter(|&i| {
                let g = &analysis.groups[i];
                model::listed(g) && category.is_none_or(|c| g.category == c)
            })
            .map(|i| file_row(d, i))
            .collect(),
        List::Folders => (0..analysis.folders.len())
            .filter(|&i| analysis.folders[i].listed())
            .map(|i| folder_row(d, i))
            .collect(),
        List::Both => unreachable!("rows of one list at a time"),
    }
}

/// A file group's row; copies inside listed identical folders are the
/// folder group's to remove.
fn file_row<'a>(d: &mut Decisions<'a>, index: usize) -> Row<'a> {
    let g = &d.analysis().groups[index];
    let (members, view) = d.file(index);
    Row {
        info: GroupInfo {
            id: g.id.clone(),
            kind: "file",
            name: JsonPath::new(&g.name),
            size: g.size,
            files: None,
            copies_total: g.copies.len(),
            category: Some(g.category),
            same_items: None,
        },
        members,
        clusters: g.clusters.clone(),
        drive: g.copies.iter().map(|c| c.drive).collect(),
        folder: None,
        view,
    }
}

/// A folder group's row; members inside a part another listed folder
/// group removes are that group's.
fn folder_row<'a>(d: &mut Decisions<'a>, index: usize) -> Row<'a> {
    let f = &d.analysis().folders[index];
    let (members, view) = d.folder(index);
    Row {
        info: GroupInfo {
            id: f.id.clone(),
            kind: if f.partial() {
                "folder_partial"
            } else {
                "folder"
            },
            name: JsonPath::new(&f.name),
            size: f.bytes,
            files: Some(f.files),
            copies_total: f.members.len(),
            category: None,
            same_items: f.partial().then_some(f.items.len()),
        },
        members,
        clusters: model::folder_clusters(f),
        drive: f.members.iter().map(|m| m.drive).collect(),
        folder: Some(f),
        view,
    }
}

fn find_row<'a>(d: &mut Decisions<'a>, id: &str) -> Option<Row<'a>> {
    let analysis = d.analysis();
    if let Some(i) = analysis.groups.iter().position(|g| g.id == id) {
        return Some(file_row(d, i));
    }
    let i = analysis.folders.iter().position(|f| f.id == id)?;
    Some(folder_row(d, i))
}

fn group_view(row: &Row<'_>, v: &View, scope: &[u8]) -> GroupView {
    let k = &row.members[v.keeper];
    let mut in_scope = Vec::new();
    let mut elsewhere: std::collections::BTreeMap<&str, u64> = Default::default();
    let mut in_identical_folders = 0;
    let mut remove_shares = std::collections::BTreeSet::new();
    let mut unmovable = Vec::new();
    for (i, m) in row.members.iter().enumerate() {
        let here = m.names.iter().all(|n| model::within(n, scope));
        match v.roles[i] {
            Role::Remove => {
                remove_shares.insert(m.share);
            }
            Role::Unmovable => unmovable.push(JsonPath::new(m.path())),
            _ => {}
        }
        match v.roles[i] {
            Role::Keep => {}
            Role::InFolder => in_identical_folders += 1,
            _ if !here => *elsewhere.entry(m.share).or_default() += 1,
            role => in_scope.push(CopyOut {
                path: JsonPath::new(m.path()),
                share: m.share.to_string(),
                mtime: time(m.mtime),
                role,
            }),
        }
    }
    let total = in_scope.len();
    in_scope.truncate(ROW_COPIES);
    GroupView {
        group: row.info.clone(),
        keeper: KeeperOut {
            path: JsonPath::new(k.path()),
            share: k.share.to_string(),
            mtime: time(k.mtime),
            reason: v.reason,
            in_scope: k.names.iter().all(|n| model::within(n, scope)),
        },
        in_scope,
        in_scope_total: total,
        removes: v.removes,
        elsewhere_by_share: elsewhere
            .into_iter()
            .map(|(share, count)| ShareCount {
                share: share.to_string(),
                count,
            })
            .collect(),
        remove_shares: remove_shares.into_iter().map(str::to_string).collect(),
        unmovable,
        in_identical_folders,
        can_free: v.can_free,
        frees_later: v.frees_later,
        touches_drive: (0..row.members.len()).any(|i| v.roles[i] == Role::Remove && row.drive[i]),
        shared: row.clusters.len() == 1,
        partial: partial_out(row, v, scope),
    }
}

/// The first folder a partly identical row deletes from in the scope,
/// compared with the kept one.
fn partial_out(row: &Row<'_>, v: &View, scope: &[u8]) -> Option<PartialOut> {
    let f = row.folder.filter(|f| f.partial())?;
    let i = (0..row.members.len())
        .filter(|&i| v.roles[i] == Role::Remove)
        .min_by_key(|&i| {
            (
                !row.members[i].names.iter().all(|n| model::within(n, scope)),
                row.members[i].path(),
            )
        })?;
    let m = &f.members[i];
    let side = m.side.as_ref()?;
    let keeper = &row.members[v.keeper];
    Some(PartialOut {
        path: JsonPath::new(&m.path),
        same_items: side.same_items,
        total_items: side.total_items,
        same_files: f.files,
        total_files: side.total_files,
        same_bytes: f.bytes,
        total_bytes: side.total_bytes,
        counterpart: JsonPath::new(keeper.path()),
    })
}

/// Which list a scope answer pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum List {
    Files,
    Folders,
    /// Large files in `groups` and identical folders in `folders`: a
    /// folder's page in one request.
    Both,
}

#[derive(Serialize)]
struct Page<T> {
    total: usize,
    offset: u32,
    items: Vec<T>,
}

#[derive(Serialize)]
struct Child {
    path: JsonPath,
    name: JsonPath,
    tally: TallyOut,
}

#[derive(Serialize)]
struct Thresholds {
    record_bytes: u64,
    list_bytes: u64,
}

#[derive(Serialize)]
struct ScopeDocument {
    #[serde(flatten)]
    envelope: Envelope,
    volume: JsonPath,
    scope: JsonPath,
    analysis: Option<AnalysisState>,
    recalculating: bool,
    thresholds: Thresholds,
    preferred_shares: Vec<String>,
    totals: Option<TallyOut>,
    children: Vec<Child>,
    unchecked: Option<Unchecked>,
    list: List,
    category: Option<Category>,
    touches_drive: bool,
    groups: Page<GroupView>,
    /// With `list` both: the identical folders (no category), paged by
    /// the same offset and limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    folders: Option<Page<GroupView>>,
}

/// What `scope` and the other answers check of a scope path.
fn scope_of(scope: Option<&Path>, mount: &[u8]) -> Result<Vec<u8>, String> {
    let scope = scope.map_or_else(|| mount.to_vec(), |p| p.as_os_str().as_bytes().to_vec());
    if scope != mount && !model::within(&scope, mount) {
        return Err("the scope must be the volume or a folder on it".into());
    }
    Ok(scope)
}

/// Arguments of `duplicates scope`.
pub struct ScopeQuery<'a> {
    pub volume: &'a Path,
    pub path: Option<&'a Path>,
    pub list: List,
    pub category: Option<Category>,
    pub offset: u32,
    pub limit: u32,
}

/// One page of a list's rows in `scope`, biggest first, with whether a
/// removed copy lies in Drive and the copies outside left alone. A group
/// whose copies here the user all kept stays listed, so the choice can be
/// undone.
fn listing(
    a: &Analysis,
    list: List,
    category: Option<Category>,
    scope: &[u8],
    settings: &Settings,
    offset: u32,
    limit: u32,
) -> (Page<GroupView>, bool) {
    let mut drive = false;
    let mut listed: Vec<GroupView> = Vec::new();
    let mut decisions = Decisions::new(a, settings);
    for row in rows(&mut decisions, list, category) {
        if !row
            .members
            .iter()
            .any(|m| m.names.iter().all(|n| model::within(n, scope)))
        {
            continue;
        }
        let v = &row.view;
        if v.removes == 0 && settings.kept(&row.info.id).is_empty() {
            continue;
        }
        let gv = group_view(&row, v, scope);
        drive |= gv.touches_drive;
        listed.push(gv);
    }
    listed.sort_by(|x, y| {
        (y.can_free, y.frees_later, y.group.size, &x.group.id).cmp(&(
            x.can_free,
            x.frees_later,
            x.group.size,
            &y.group.id,
        ))
    });
    let page = Page {
        total: listed.len(),
        offset,
        items: listed
            .into_iter()
            .skip(offset as usize)
            .take(limit.min(MAX_PAGE) as usize)
            .collect(),
    };
    (page, drive)
}

/// `duplicates scope VOLUME`: totals, child folders and one page of
/// groups for a scope.
pub fn scope(q: ScopeQuery<'_>, json: bool, state_dir: &Path) -> Result<i32, Failure> {
    let mut doc = ScopeDocument {
        envelope: Envelope::new("duplicates-scope", SystemTime::now()),
        volume: JsonPath::new(q.volume.as_os_str().as_bytes()),
        scope: JsonPath::new(q.path.unwrap_or(q.volume).as_os_str().as_bytes()),
        analysis: None,
        recalculating: false,
        thresholds: Thresholds {
            record_bytes: candidates::MIN_SIZE,
            list_bytes: model::LIST_MIN_SIZE,
        },
        preferred_shares: Vec::new(),
        totals: None,
        children: Vec::new(),
        unchecked: None,
        list: q.list,
        category: q.category,
        touches_drive: false,
        groups: Page {
            total: 0,
            offset: q.offset,
            items: Vec::new(),
        },
        folders: (q.list == List::Both).then(|| Page {
            total: 0,
            offset: q.offset,
            items: Vec::new(),
        }),
    };
    let result = (|| -> Result<(), String> {
        let loaded = load(q.volume, state_dir).map_err(|e| e.to_string())?;
        let scope = scope_of(q.path, &loaded.volume.mount)?;
        doc.recalculating = recalculating(&loaded);
        doc.preferred_shares = loaded.settings.preferred_shares.clone();
        doc.analysis = Some(loaded.state.clone());
        let Some(a) = &loaded.analysis else {
            return Ok(());
        };
        let settings = &loaded.settings;
        doc.totals = Some(TallyOut::of(&a.tally(&scope, settings)));
        let mut children: Vec<Child> = a
            .child_tallies(&scope, settings)
            .into_iter()
            .map(|(dir, t)| Child {
                name: JsonPath::new(model::last_name(&dir)),
                path: JsonPath::new(&dir),
                tally: TallyOut::of(&t),
            })
            .collect();
        children.sort_by(|x, y| {
            (y.tally.all.can_free, y.tally.all.frees_later, &x.name.path).cmp(&(
                x.tally.all.can_free,
                x.tally.all.frees_later,
                &y.name.path,
            ))
        });
        doc.children = children;
        doc.unchecked = Some(a.unchecked.clone());
        let mut page = |list: List, category: Option<Category>| {
            let (page, drive) = listing(a, list, category, &scope, settings, q.offset, q.limit);
            doc.touches_drive |= drive;
            page
        };
        let (groups, folders) = if q.list == List::Both {
            (
                page(List::Files, q.category),
                Some(page(List::Folders, None)),
            )
        } else {
            (page(q.list, q.category), None)
        };
        doc.groups = groups;
        doc.folders = folders;
        Ok(())
    })();
    if let Err(message) = result {
        fail(&mut doc.envelope, q.volume, message);
    }
    print(json, &doc, || {
        let mut out = String::new();
        for g in &doc.groups.items {
            out.push_str(&format!(
                "{}\t{} copies\tcan free {}\tkeeps {}\n",
                g.group.name.path,
                g.group.copies_total,
                crate::output::human_bytes(g.can_free),
                g.keeper.path.path
            ));
        }
        out
    })
}

#[derive(Serialize)]
struct CopyRow {
    path: JsonPath,
    share: String,
    mtime: String,
    role: Role,
    in_scope: bool,
}

#[derive(Serialize)]
struct CopiesDocument {
    #[serde(flatten)]
    envelope: Envelope,
    volume: JsonPath,
    scope: JsonPath,
    group: Option<GroupView>,
    copies: Page<CopyRow>,
    /// Partly identical folders: the identical items, largest first,
    /// paged by the same offset and limit (the folders are few).
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<Page<ItemRow>>,
}

/// `duplicates copies VOLUME --group=ID`: every copy of one group with
/// its role in the scope, by share: the scope's share first, then the
/// kept copy's, then the rest by name. For partly identical folders, also
/// one page of the identical items.
#[allow(clippy::too_many_arguments)]
pub fn copies(
    volume_path: &Path,
    group: &str,
    path: Option<&Path>,
    offset: u32,
    limit: u32,
    json: bool,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let mut doc = CopiesDocument {
        envelope: Envelope::new("duplicates-copies", SystemTime::now()),
        volume: JsonPath::new(volume_path.as_os_str().as_bytes()),
        scope: JsonPath::new(path.unwrap_or(volume_path).as_os_str().as_bytes()),
        group: None,
        copies: Page {
            total: 0,
            offset,
            items: Vec::new(),
        },
        items: None,
    };
    let result = (|| -> Result<(), String> {
        let loaded = load(volume_path, state_dir).map_err(|e| e.to_string())?;
        let scope = scope_of(path, &loaded.volume.mount)?;
        let a = loaded
            .analysis
            .as_ref()
            .ok_or("no duplicate analysis yet")?;
        let row = find_row(&mut Decisions::new(a, &loaded.settings), group)
            .ok_or("no such duplicate group")?;
        let v = &row.view;
        let scope_share = model::share_of(&scope, &loaded.volume.mount);
        let keeper_share = row.members[v.keeper].share;
        let mut order: Vec<usize> = (0..row.members.len()).collect();
        order.sort_by_key(|&i| {
            let m = &row.members[i];
            let rank = if !scope_share.is_empty() && m.share == scope_share {
                0
            } else if m.share == keeper_share {
                1
            } else {
                2
            };
            (rank, m.share, m.path())
        });
        doc.copies.total = order.len();
        doc.copies.items = order
            .into_iter()
            .skip(offset as usize)
            .take(limit.min(MAX_PAGE) as usize)
            .map(|i| {
                let m = &row.members[i];
                CopyRow {
                    path: JsonPath::new(m.path()),
                    share: m.share.to_string(),
                    mtime: time(m.mtime),
                    role: v.roles[i],
                    in_scope: m.names.iter().all(|n| model::within(n, &scope)),
                }
            })
            .collect();
        doc.items = row
            .folder
            .filter(|f| f.partial())
            .map(|f| items_page(f, offset, limit));
        doc.group = Some(group_view(&row, v, &scope));
        Ok(())
    })();
    if let Err(message) = result {
        fail(&mut doc.envelope, volume_path, message);
    }
    print(json, &doc, || {
        doc.copies
            .items
            .iter()
            .map(|c| format!("{:?}\t{}\n", c.role, c.path.path))
            .collect()
    })
}

#[derive(Serialize)]
struct ItemRow {
    /// Below the folder, without a leading `/`.
    path: JsonPath,
    /// `folder` or `file`.
    kind: &'static str,
    files: u64,
    bytes: u64,
}

/// The identical items of partly identical folders `f`, largest first:
/// one page of `offset` and `limit`.
fn items_page(f: &model::FolderGroup, offset: u32, limit: u32) -> Page<ItemRow> {
    let mut order: Vec<&model::Item> = f.items.iter().collect();
    order.sort_by(|x, y| y.bytes.cmp(&x.bytes).then(x.rel.cmp(&y.rel)));
    Page {
        total: order.len(),
        offset,
        items: order
            .into_iter()
            .skip(offset as usize)
            .take(limit.min(MAX_PAGE) as usize)
            .map(|i| ItemRow {
                path: JsonPath::new(&i.rel),
                kind: if i.folder { "folder" } else { "file" },
                files: i.files,
                bytes: i.bytes,
            })
            .collect(),
    }
}

#[derive(Serialize)]
struct KeepDocument {
    #[serde(flatten)]
    envelope: Envelope,
    volume: JsonPath,
    scope: JsonPath,
    group: Option<GroupView>,
    totals: Option<TallyOut>,
}

/// `duplicates keep VOLUME --group=ID --path=COPY --keep=on|off`: the
/// user keeps a copy, or stops keeping it. Only the tallies change; the
/// analysis is not run again.
#[allow(clippy::too_many_arguments)]
pub fn keep(
    volume_path: &Path,
    group: &str,
    copy: &Path,
    on: bool,
    scope_path: Option<&Path>,
    json: bool,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let mut doc = KeepDocument {
        envelope: Envelope::new("duplicates-keep", SystemTime::now()),
        volume: JsonPath::new(volume_path.as_os_str().as_bytes()),
        scope: JsonPath::new(scope_path.unwrap_or(volume_path).as_os_str().as_bytes()),
        group: None,
        totals: None,
    };
    let result = (|| -> Result<(), String> {
        let mut loaded = load(volume_path, state_dir).map_err(|e| e.to_string())?;
        let scope = scope_of(scope_path, &loaded.volume.mount)?;
        let copy = copy.as_os_str().as_bytes().to_vec();
        let mut a = loaded.analysis.take().ok_or("no duplicate analysis yet")?;
        {
            let row = find_row(&mut Decisions::new(&a, &loaded.settings), group)
                .ok_or("no such duplicate group")?;
            if !row.members.iter().any(|m| m.path() == copy.as_slice()) {
                return Err("the path is not a copy of this group".into());
            }
        }
        let settings = &mut loaded.settings;
        let kept = settings.keep.entry(group.to_string()).or_default();
        kept.retain(|p| *p != copy);
        if on {
            kept.push(copy);
            kept.sort();
        }
        if kept.is_empty() {
            settings.keep.remove(group);
        }
        save_settings(state_dir, &loaded.volume.fsid, settings).map_err(|e| e.to_string())?;
        // The preferred shares of the analysis stay those it was made with.
        let tally_settings = Settings {
            keep: settings.keep.clone(),
            preferred_shares: a.preferred_shares.clone(),
        };
        a.retally(&tally_settings);
        // Only the newest report's own results are rewritten.
        if let Some((report, _)) = &loaded.volume.report
            && loaded.results.as_deref() == Some(report.as_path())
        {
            save_results(report, &mut a).map_err(|e| e.to_string())?;
        }
        let row = find_row(&mut Decisions::new(&a, &tally_settings), group).expect("checked above");
        doc.group = Some(group_view(&row, &row.view, &scope));
        doc.totals = Some(TallyOut::of(&a.tally(&scope, &tally_settings)));
        Ok(())
    })();
    if let Err(message) = result {
        fail(&mut doc.envelope, volume_path, message);
    }
    print(json, &doc, String::new)
}

#[derive(Serialize)]
struct PreferredDocument {
    #[serde(flatten)]
    envelope: Envelope,
    volume: JsonPath,
    shares: Vec<String>,
    /// The tallies still use other shares: run the analysis again.
    recalculating: bool,
}

/// `duplicates preferred-shares VOLUME [--shares=JSON]`: reads or
/// replaces the ordered shares whose copies stay first. A change marks
/// the numbers `recalculating` until the analysis runs again.
pub fn preferred_shares(
    volume_path: &Path,
    shares: Option<&str>,
    json: bool,
    state_dir: &Path,
) -> Result<i32, Failure> {
    let mut doc = PreferredDocument {
        envelope: Envelope::new("duplicates-preferred-shares", SystemTime::now()),
        volume: JsonPath::new(volume_path.as_os_str().as_bytes()),
        shares: Vec::new(),
        recalculating: false,
    };
    let result = (|| -> Result<(), String> {
        let mut loaded = load(volume_path, state_dir).map_err(|e| e.to_string())?;
        if let Some(text) = shares {
            let list = parse_shares(text)?;
            loaded.settings.preferred_shares = list;
            save_settings(state_dir, &loaded.volume.fsid, &loaded.settings)
                .map_err(|e| e.to_string())?;
        }
        doc.shares = loaded.settings.preferred_shares.clone();
        doc.recalculating = recalculating(&loaded);
        Ok(())
    })();
    if let Err(message) = result {
        fail(&mut doc.envelope, volume_path, message);
    }
    print(json, &doc, || doc.shares.join("\n") + "\n")
}

/// A JSON list of at most [`MAX_PREFERRED`] distinct share names.
pub fn parse_shares(text: &str) -> Result<Vec<String>, String> {
    let list: Vec<String> =
        serde_json::from_str(text).map_err(|e| format!("shares must be a JSON list: {e}"))?;
    if list.len() > MAX_PREFERRED {
        return Err(format!("at most {MAX_PREFERRED} preferred shares"));
    }
    let mut seen = std::collections::HashSet::new();
    for s in &list {
        if s.is_empty()
            || s.len() > 255
            || s.contains('/')
            || s.starts_with('@')
            || s.chars().any(char::is_control)
            || !seen.insert(s)
        {
            return Err(format!("not a share name: {s:?}"));
        }
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_shares_are_distinct_plain_names() {
        assert_eq!(
            parse_shares(r#"["photo","video"]"#).unwrap(),
            ["photo", "video"]
        );
        for bad in [
            r#"["a/b"]"#,
            r#"[""]"#,
            r#"["@appstore"]"#,
            r#"["a","a"]"#,
            r#"{"a":1}"#,
        ] {
            assert!(parse_shares(bad).is_err(), "{bad}");
        }
    }

    const MIB: u64 = 1 << 20;

    /// Identical folders `trip` in shares a and b, with their stamps.
    fn analysis() -> Analysis {
        let members = ["/v/a/trip", "/v/b/trip"]
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let mut m = model::FolderMember::new(p.as_bytes(), b"/v", 1, 25 * MIB);
                m.cluster = k as u32;
                m.mtime = k as u64;
                m.oldest = k as u64;
                m.dirs = vec![format!("{p}/d").into_bytes()];
                m.file_stamps = vec![model::FileStamp {
                    rel: b"/d/x".to_vec(),
                    stamp: model::Stamp::default(),
                }];
                m
            })
            .collect();
        let mut a = Analysis {
            format: model::FORMAT_INLINE_STAMPS,
            mount: b"/v".to_vec(),
            ..Analysis::default()
        };
        a.folders.push(model::FolderGroup {
            id: "ftrip".into(),
            name: b"trip".to_vec(),
            files: 1,
            bytes: 25 * MIB,
            covered: 0,
            members,
            clusters: vec![
                Cluster {
                    free: 25 * MIB,
                    ..Cluster::default()
                },
                Cluster {
                    free: 25 * MIB,
                    ..Cluster::default()
                },
            ],
            items: Vec::new(),
            in_folders: false,
            stamps: None,
        });
        a
    }

    fn code(e: &Failure) -> Option<&'static str> {
        e.downcast_ref::<crate::guard::Refusal>().map(|r| r.code)
    }

    /// Results with the stamps inline are split on first read: the page
    /// reads small results, and a plan gets the stamps back.
    #[test]
    fn inline_results_are_split_on_first_read() {
        let dir = crate::spill::testing::scratch("dup-split");
        let report = dir.join("r.srrep");
        let original = analysis();
        std::fs::write(
            results_path(&report),
            serde_json::to_vec(&original).unwrap(),
        )
        .unwrap();
        let mut a = load_results(&report).unwrap();
        assert_eq!(a.format, model::FORMAT);
        let saved = std::fs::read_to_string(results_path(&report)).unwrap();
        assert!(!saved.contains("file_stamps") && saved.contains(&a.stamps_id));
        assert!(a.folders[0].members[0].file_stamps.is_empty());
        // Read again: already split, nothing rewritten.
        let again = load_results(&report).unwrap();
        assert_eq!(again.stamps_id, a.stamps_id);
        attach_stamps(Some(&report), &mut a, &["ftrip".into()]).unwrap();
        assert_eq!(a.folders[0].members, original.folders[0].members);
    }

    /// A plan never runs on missing or damaged stamps: it asks for the
    /// analysis again.
    #[test]
    fn missing_or_damaged_stamps_refuse_the_plan() {
        let dir = crate::spill::testing::scratch("dup-stamps-refuse");
        let report = dir.join("r.srrep");
        std::fs::write(
            results_path(&report),
            serde_json::to_vec(&analysis()).unwrap(),
        )
        .unwrap();
        let a = load_results(&report).unwrap();
        let stamps = stamps::path_of(&report);
        let mut bytes = std::fs::read(&stamps).unwrap();
        bytes[3] ^= 1;
        std::fs::write(&stamps, &bytes).unwrap();
        let e = attach_stamps(Some(&report), &mut a.clone(), &["ftrip".into()]).unwrap_err();
        assert_eq!(code(&e), Some("duplicates_stale"), "{e}");
        std::fs::remove_file(&stamps).unwrap();
        let e = attach_stamps(Some(&report), &mut a.clone(), &["ftrip".into()]).unwrap_err();
        assert_eq!(code(&e), Some("duplicates_stale"), "{e}");
        // Groups not selected need no stamps.
        attach_stamps(Some(&report), &mut a.clone(), &["other".into()]).unwrap();
    }

    /// A row whose copies the user all kept stays listed with nothing to
    /// remove, so the choice can be undone; other rows with nothing to
    /// remove are not listed. Keeping only the copy in the scope still
    /// removes the one in another share: a group is seen on its volume.
    #[test]
    fn a_row_the_user_kept_whole_stays_listed() {
        let mut a = analysis();
        a.format = model::FORMAT;
        let settings = Settings::default();
        a.retally(&settings);
        let (page, _) = listing(&a, List::Folders, None, b"/v/a", &settings, 0, 50);
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].removes, 1);
        let here = Settings {
            keep: [("ftrip".to_string(), vec![b"/v/a/trip".to_vec()])].into(),
            ..Settings::default()
        };
        a.retally(&here);
        let (page, _) = listing(&a, List::Folders, None, b"/v/a", &here, 0, 50);
        assert_eq!((page.total, page.items[0].removes), (1, 1));
        assert_eq!(page.items[0].remove_shares, ["b"]);
        let kept = Settings {
            keep: [(
                "ftrip".to_string(),
                vec![b"/v/a/trip".to_vec(), b"/v/b/trip".to_vec()],
            )]
            .into(),
            ..Settings::default()
        };
        a.retally(&kept);
        let (page, _) = listing(&a, List::Folders, None, b"/v/a", &kept, 0, 50);
        assert_eq!((page.total, page.items[0].removes), (1, 0));
        let (page, _) = listing(&a, List::Files, None, b"/v/a", &kept, 0, 50);
        assert_eq!(page.total, 0);
    }

    #[test]
    fn a_scope_is_the_volume_or_below_it() {
        assert_eq!(scope_of(None, b"/volume1").unwrap(), b"/volume1");
        assert!(scope_of(Some(Path::new("/volume1/photo")), b"/volume1").is_ok());
        assert!(scope_of(Some(Path::new("/volume10/photo")), b"/volume1").is_err());
    }
}

/// What `cleanup plan --duplicates` adds to the plan document.
#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    /// Copies removed.
    pub copies: u64,
    /// Groups with a copy removed.
    pub groups: u64,
    /// Copies kept, checked again before anything is removed.
    pub kept: u64,
    /// When the scan the analysis used finished (Unix seconds): the
    /// removed copies are as that scan saw them.
    pub scan_finished: u64,
}

/// A duplicates selection expanded for the cleanup plan.
pub struct Expansion {
    pub paths: Vec<PathBuf>,
    /// The group each of `paths` is a copy of, by index.
    pub groups: Vec<String>,
    /// What stays: each must still exist when the others move.
    pub keep: Vec<crate::recycle_move::Keeper>,
    pub summary: PlanSummary,
}

/// `cleanup plan --duplicates=JSON`: `{volume, groups: [id…]}`,
/// paths as text or `{path, path_hex}`.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    volume: PathText,
    groups: Vec<String>,
}

/// A path as text or as printed: `{path, path_hex?}`.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum PathText {
    Text(String),
    Printed(crate::output::JsonPath),
}

impl PathText {
    fn bytes(&self) -> Vec<u8> {
        match self {
            PathText::Text(t) => t.as_bytes().to_vec(),
            PathText::Printed(p) => p.bytes(),
        }
    }
}

fn refusal(code: &'static str, message: String) -> Failure {
    Box::new(crate::guard::Refusal::new(
        code,
        format!("{code}: {message}"),
    ))
}

/// `keeper_conflict`: a selected group removes `c.copy`, which the user
/// kept in another group.
/// The error names the copy as its path and both groups.
pub(crate) fn keeper_conflict_refusal(c: &model::KeepConflict) -> Failure {
    let group = |(id, name): &(String, Vec<u8>)| crate::output::GroupName {
        id: id.clone(),
        name: JsonPath::new(name),
    };
    let message = format!(
        "keeper_conflict: {} is kept in {}, but removing {} would remove it: select fewer groups, or keep another copy",
        crate::output::escape_human(&c.copy),
        crate::output::escape_human(&c.kept_in.1),
        crate::output::escape_human(&c.removed_by.1),
    );
    let refusal = crate::guard::Refusal::new("keeper_conflict", message).with_conflict(
        crate::output::KeeperConflict {
            kept_in: group(&c.kept_in),
            removed_by: group(&c.removed_by),
        },
    );
    Box::new(crate::guard::AtPath {
        path: c.copy.clone(),
        source: Box::new(refusal),
    })
}

/// The copies the keep rule moves to the Recycle Bin from each selected
/// group in the scope, and the copies it keeps. A folder group moves only
/// the large files the analysis matched by checksum inside each removed
/// folder; its other files stay. Each kept copy must still exist, a file
/// with the size the analysis read; a moved copy can be restored.
pub fn expand(spec: &str, state_dir: &Path) -> Result<Expansion, Failure> {
    use crate::recycle_move::Keeper;
    let sel: Selection = serde_json::from_str(spec)
        .map_err(|e| refusal("duplicates_invalid", format!("selection: {e}")))?;
    let volume_bytes = sel.volume.bytes();
    let volume_path = Path::new(std::ffi::OsStr::from_bytes(&volume_bytes));
    let mut loaded = load(volume_path, state_dir)?;
    if let Some(a) = loaded.analysis.as_mut() {
        attach_stamps(loaded.results.as_deref(), a, &sel.groups)?;
    }
    let analysis = loaded.analysis.as_ref().ok_or_else(|| {
        refusal(
            "duplicates_stale",
            "no duplicate analysis of this volume".into(),
        )
    })?;
    if loaded.state.state != "ready" {
        return Err(refusal(
            "duplicates_stale",
            "a newer scan exists: run the duplicate analysis again".into(),
        ));
    }
    // A copy in a folder below the volume that is no DSM shared folder
    // goes like any other: the plan's check of each path marks it to be
    // removed for good, which apply does only once that is confirmed
    // (user decision 2026-11-14).
    // Per selected group: what goes, and what stays.
    struct Selected<'a> {
        id: &'a str,
        name: &'a [u8],
        removed: Vec<Copy<'a>>,
        survivors: Vec<Copy<'a>>,
    }
    #[derive(Clone, Copy)]
    enum Copy<'a> {
        File(&'a model::CopyRecord),
        Folder(&'a model::FolderGroup, &'a model::FolderMember),
    }
    impl Copy<'_> {
        /// What removing it removes: a file's names, a folder, or the
        /// identical items of partly identical folders.
        fn paths(&self) -> Vec<Vec<u8>> {
            match self {
                Copy::File(c) => c.names.clone(),
                Copy::Folder(f, m) => f.parts(m).into_iter().map(|(p, _, _)| p).collect(),
            }
        }
    }
    let mut summary = PlanSummary {
        copies: 0,
        groups: 0,
        kept: 0,
        scan_finished: loaded
            .volume
            .report
            .as_ref()
            .map_or(0, |(_, header)| header.scan_finished),
    };
    let mut selected: Vec<Selected<'_>> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // Each group found by its ID once: a selection of any size is linear.
    let mut decisions = Decisions::new(analysis, &loaded.settings);
    let files_by_id: std::collections::HashMap<&str, (usize, &model::Group)> = analysis
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| (g.id.as_str(), (i, g)))
        .collect();
    let folders_by_id: std::collections::HashMap<&str, (usize, &model::FolderGroup)> = analysis
        .folders
        .iter()
        .enumerate()
        .map(|(i, f)| (f.id.as_str(), (i, f)))
        .collect();
    for id in &sel.groups {
        if !seen.insert(id.as_str()) {
            continue;
        }
        let row = match (files_by_id.get(id.as_str()), folders_by_id.get(id.as_str())) {
            (Some((i, _)), _) => file_row(&mut decisions, *i),
            (None, Some((i, _))) => folder_row(&mut decisions, *i),
            (None, None) => {
                return Err(refusal(
                    "duplicates_unknown_group",
                    format!("no group {id}"),
                ));
            }
        };
        let v = &row.view;
        if v.removes == 0 {
            continue;
        }
        let copies: Vec<Copy<'_>> = match files_by_id.get(id.as_str()) {
            Some((_, g)) => g.copies.iter().map(Copy::File).collect(),
            None => folders_by_id
                .get(id.as_str())
                .map(|(_, f)| f.members.iter().map(|m| Copy::Folder(f, m)).collect())
                .unwrap_or_default(),
        };
        let (mut removed, mut survivors) = (Vec::new(), Vec::new());
        for (i, c) in copies.into_iter().enumerate() {
            if v.roles[i] == Role::Remove {
                removed.push(c);
            } else {
                survivors.push(c);
            }
        }
        if removed.is_empty() {
            continue;
        }
        summary.groups += 1;
        summary.copies += removed.len() as u64;
        selected.push(Selected {
            id: id.as_str(),
            name: files_by_id
                .get(id.as_str())
                .map(|(_, g)| g.name.as_slice())
                .or_else(|| {
                    folders_by_id
                        .get(id.as_str())
                        .map(|(_, f)| f.name.as_slice())
                })
                .unwrap_or_default(),
            removed,
            survivors,
        });
    }
    // Every path the plan removes; a folder copy goes whole, partly
    // identical folders only their identical items.
    let mut removed_paths: Vec<Vec<u8>> = selected
        .iter()
        .flat_map(|g| g.removed.iter().flat_map(|c| c.paths()))
        .collect();
    removed_paths.sort();
    removed_paths.dedup();
    // A path goes when it or a folder above it is removed: a lookup per
    // folder of the path, not a pass over every removed path.
    let removed: std::collections::HashSet<&[u8]> =
        removed_paths.iter().map(Vec::as_slice).collect();
    let goes = |path: &[u8]| {
        removed.contains(path)
            || path
                .iter()
                .enumerate()
                .filter(|(i, b)| **b == b'/' && *i > 0)
                .any(|(i, _)| removed.contains(&path[..i]))
    };
    // A copy the user kept in any group never goes with another group.
    let removing: Vec<model::Removal<'_>> = selected
        .iter()
        .map(|g| model::Removal {
            id: g.id,
            name: g.name,
            paths: g.removed.iter().flat_map(|c| c.paths()).collect(),
        })
        .collect();
    if let Some(c) = model::keep_conflict(analysis, &loaded.settings, &removing) {
        return Err(keeper_conflict_refusal(&c));
    }
    // Each group keeps at least one copy this plan leaves: its keeper, a
    // copy the user kept, one outside the scope or inside a kept folder.
    let mut keep: Vec<Copy<'_>> = Vec::new();
    for g in &selected {
        let left: Vec<Copy<'_>> = g
            .survivors
            .iter()
            .copied()
            .filter(|c| c.paths().iter().all(|p| !goes(p)))
            .collect();
        if left.is_empty() {
            return Err(refusal(
                "keeper_removed",
                format!(
                    "no copy of {} would be left: another selected group removes the one kept; select fewer groups",
                    crate::output::escape_human(g.name)
                ),
            ));
        }
        keep.extend(left);
    }
    let mut expansion = Expansion {
        paths: Vec::new(),
        groups: Vec::new(),
        keep: Vec::new(),
        summary,
    };
    // What stays must still exist, a file with its analysed size; the
    // moved copies go to the Recycle Bin, where they can be restored.
    let mut kept_seen = std::collections::HashSet::new();
    for c in &keep {
        match c {
            Copy::File(f) => {
                if kept_seen.insert(f.path().to_vec()) {
                    expansion.keep.push(Keeper {
                        path: f.path().to_vec(),
                        size: Some(f.stamp.size),
                    });
                }
            }
            Copy::Folder(_, m) => {
                if !kept_seen.insert(m.path.clone()) {
                    continue;
                }
                expansion.keep.push(Keeper {
                    path: m.path.clone(),
                    size: None,
                });
                for f in &m.file_stamps {
                    expansion.keep.push(Keeper {
                        path: [m.path.as_slice(), &f.rel].concat(),
                        size: Some(f.stamp.size),
                    });
                }
            }
        }
    }
    crate::planning::step(crate::planning::Step::Kept);
    crate::recycle_move::check_keepers(&expansion.keep).map_err(|e| Box::new(e) as Failure)?;
    expansion.summary.kept = kept_seen.len() as u64;
    for g in &selected {
        for c in &g.removed {
            match c {
                Copy::File(f) => {
                    for name in &f.names {
                        expansion
                            .paths
                            .push(PathBuf::from(std::ffi::OsStr::from_bytes(name)));
                    }
                }
                Copy::Folder(f, m) => {
                    for (path, _, _) in f.parts(m) {
                        expansion
                            .paths
                            .push(PathBuf::from(std::ffi::OsStr::from_bytes(&path)));
                    }
                }
            }
        }
        expansion
            .groups
            .resize(expansion.paths.len(), g.id.to_owned());
    }
    if expansion.paths.is_empty() {
        return Err(refusal(
            "duplicates_nothing",
            "the selected groups have no copy to remove".into(),
        ));
    }
    Ok(expansion)
}

#[cfg(test)]
mod live {
    use super::*;

    /// On the fixture of the test below: a file inside the kept copy of the
    /// first listed folder group that changes size makes the plan refuse.
    /// It edits the fixture: run it last.
    #[test]
    #[ignore = "needs a scanned Btrfs fixture and root; edits it"]
    fn a_kept_file_that_changes_size_refuses_the_plan() {
        let volume = std::env::var("SYNORECLAIM_DUP_VOLUME").unwrap();
        let state = PathBuf::from(std::env::var("SYNORECLAIM_DUP_STATE").unwrap());
        let loaded = load(Path::new(&volume), &state).unwrap();
        let a = loaded.analysis.unwrap();
        let f = a.folders.iter().find(|f| f.listed()).unwrap();
        let spec = serde_json::json!({"volume": volume, "groups": [f.id]}).to_string();
        let e = expand(&spec, &state).unwrap();
        let target = e.keep.iter().find(|k| k.size.is_some()).unwrap();
        let path = Path::new(std::ffi::OsStr::from_bytes(&target.path));
        let mut bytes = std::fs::read(path).unwrap();
        bytes.push(0);
        std::fs::write(path, &bytes).unwrap();
        let err = expand(&spec, &state).err().unwrap();
        let code = err.downcast_ref::<crate::guard::Refusal>().map(|r| r.code);
        assert_eq!(code, Some("keeper_changed"), "{err}");
        // And a plan made before the change refuses at apply.
        assert_eq!(
            crate::recycle_move::check_keepers(&e.keep)
                .unwrap_err()
                .code,
            "keeper_changed"
        );
    }

    /// On a scanned and analysed fixture volume
    /// (`SYNORECLAIM_DUP_VOLUME`, state in `SYNORECLAIM_DUP_STATE`): the
    /// first group of the volume expands into its removable copies and
    /// its keeper, and every kept path stays out of the
    /// removed ones.
    #[test]
    #[ignore = "needs a scanned Btrfs fixture and root"]
    fn the_first_group_expands_into_removals_and_keepers() {
        let volume = std::env::var("SYNORECLAIM_DUP_VOLUME").unwrap();
        let state = PathBuf::from(std::env::var("SYNORECLAIM_DUP_STATE").unwrap());
        let loaded = load(Path::new(&volume), &state).unwrap();
        let a = loaded.analysis.unwrap();
        let id = &a.groups.iter().find(|g| model::listed(g)).unwrap().id;
        let spec = serde_json::json!({"volume": volume, "groups": [id]}).to_string();
        let e = expand(&spec, &state).unwrap();
        eprintln!(
            "{:?} remove {:?} keep {:?}",
            e.summary,
            e.paths,
            e.keep
                .iter()
                .map(|k| String::from_utf8_lossy(&k.path))
                .collect::<Vec<_>>()
        );
        assert!(!e.paths.is_empty() && !e.keep.is_empty());
        crate::recycle_move::check_keepers(&e.keep).unwrap();
        for k in &e.keep {
            assert!(e.paths.iter().all(|p| p.as_os_str().as_bytes() != k.path));
        }
    }
}
