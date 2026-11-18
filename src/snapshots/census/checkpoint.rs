//! Durable completed-share checkpoints. An interrupted share is read again;
//! completed shares are reused only with identical root identities and inputs.
use super::*;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, Read};

#[derive(Serialize, Deserialize)]
struct Saved {
    key: String,
    live_generation: Option<u64>,
    share: ShareAnalysis,
    holdings: Vec<Holding>,
    stats: AnalysisStats,
    checksum: u64,
}

fn checksum(
    key: &str,
    live_generation: Option<u64>,
    share: &ShareAnalysis,
    holdings: &[Holding],
    stats: &AnalysisStats,
) -> Result<u64, WalkError> {
    struct Hash(crate::codec::Fnv);
    impl std::io::Write for Hash {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut out = Hash(crate::codec::Fnv::new());
    serde_json::to_writer(&mut out, &(key, live_generation, share, holdings, stats))
        .map_err(io::Error::other)?;
    Ok(out.0.value())
}

fn key(set: &SubvolumeSet, share: &ShareAnalysis, request: &Request) -> String {
    // Include the whole inventory: new/deleted roots can change implicit owners.
    let identities: Vec<_> = set
        .subvolumes
        .iter()
        .map(|s| {
            (
                s.id,
                s.uuid,
                s.parent_uuid,
                s.readonly,
                s.pending_deletion,
                s.readonly.then_some(s.generation),
            )
        })
        .collect();
    format!(
        "v2:{request:?}:{share:?}:{identities:?}:{:?}",
        set.relocating
    )
}

pub(super) fn load(
    dir: &Path,
    set: &SubvolumeSet,
    share: &ShareAnalysis,
    request: &Request,
    boundary: &mut dyn BatchBoundary,
) -> Result<Option<(ShareAnalysis, Vec<Holding>, AnalysisStats)>, WalkError> {
    let path = dir.join(format!("{}.json", share.live));
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let bytes = file.metadata()?.len();
    reserve(boundary, bytes.saturating_mul(4))?;
    if bytes > 32 << 20 {
        return Err(WalkError::Checkpoint(
            "snapshot checkpoint is too large".into(),
        ));
    }
    let saved: Saved = serde_json::from_reader(std::io::BufReader::new(file.take(32 << 20)))
        .map_err(|e| WalkError::Checkpoint(format!("damaged snapshot checkpoint: {e}")))?;
    if saved.checksum
        != checksum(
            &saved.key,
            saved.live_generation,
            &saved.share,
            &saved.holdings,
            &saved.stats,
        )?
    {
        return Err(WalkError::Checkpoint(
            "snapshot checkpoint checksum mismatch".into(),
        ));
    }
    if saved.key != key(set, share, request) {
        return Err(WalkError::Checkpoint(
            "snapshot roots or analysis settings changed".into(),
        ));
    }
    // An active live tree is allowed to advance between attempts. Re-read this
    // share instead of reusing a stale comparison or rejecting stable shares.
    if saved.live_generation != set.get(share.live).map(|s| s.generation) {
        return Ok(None);
    }
    Ok(Some((saved.share, saved.holdings, saved.stats)))
}

pub(super) fn save(
    dir: &Path,
    set: &SubvolumeSet,
    planned: &ShareAnalysis,
    request: &Request,
    share: &ShareAnalysis,
    holdings: &[Holding],
    stats: &AnalysisStats,
) -> Result<(), WalkError> {
    #[derive(Serialize)]
    struct Borrowed<'a> {
        key: String,
        live_generation: Option<u64>,
        share: &'a ShareAnalysis,
        holdings: &'a [Holding],
        stats: &'a AnalysisStats,
        checksum: u64,
    }
    let saved = Borrowed {
        key: key(set, planned, request),
        live_generation: set.get(planned.live).map(|s| s.generation),
        share,
        holdings,
        stats,
        checksum: checksum(
            &key(set, planned, request),
            set.get(planned.live).map(|s| s.generation),
            share,
            holdings,
            stats,
        )?,
    };
    crate::state::replace_file(dir, &format!("{}.json", share.live), |out| {
        serde_json::to_writer(out, &saved).map_err(io::Error::other)
    })
    .map_err(|e| WalkError::Spill(io::Error::other(e)))?;
    Ok(())
}
