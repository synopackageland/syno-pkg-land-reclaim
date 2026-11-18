//! Lossless cached TopAnswer codec; public JSON continues through the existing renderers.
use super::pages::{decode_entry, encode_entry};
use crate::dsm;
use crate::model::accounting::Held;
use crate::model::tree::Metrics;
use crate::query::{Ranking, TopAnswer};
use crate::summary::{PackageOverlap, PackageRoot, ServiceSummary, ServiceUse};
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Serialize, Deserialize)]
struct Cached {
    target: Vec<u8>,
    entries: Vec<Vec<u8>>,
    hidden: u64,
    services: Vec<Service>,
    other_snapshots: u64,
    overlaps: Vec<Overlap>,
    roots: Vec<Root>,
}
#[derive(Serialize, Deserialize)]
struct Service {
    id: Option<u16>,
    path: Option<Vec<u8>>,
    paths: u64,
    metrics: Metrics,
    held: [u64; 6],
    entry: Option<Vec<u8>>,
    listed: Vec<Vec<u8>>,
    snapshots: u64,
    included_in_rows: bool,
    standalone: Option<Box<Service>>,
    common_path: Vec<u8>,
    excluded_paths: Vec<Vec<u8>>,
    full_parts: Vec<Service>,
    covering_folder: Option<Vec<u8>>,
}
#[derive(Serialize, Deserialize)]
struct Overlap {
    target: Vec<u8>,
    id: u16,
    path: Option<Vec<u8>>,
    bytes: u64,
}
#[derive(Serialize, Deserialize)]
struct Root {
    path: Vec<u8>,
    id: u16,
    marker: Option<Vec<u8>>,
}

pub fn encode(answer: &TopAnswer) -> io::Result<Vec<u8>> {
    let cached = Cached {
        target: encode_entry(&answer.ranking.target)?,
        entries: answer
            .ranking
            .entries
            .iter()
            .map(encode_entry)
            .collect::<io::Result<_>>()?,
        hidden: answer.ranking.hidden_system,
        services: answer
            .services
            .uses
            .iter()
            .map(encode_service)
            .collect::<io::Result<_>>()?,
        other_snapshots: answer.services.other_snapshots_not_read,
        overlaps: answer
            .overlaps
            .iter()
            .map(|s| Overlap {
                target: s.target.clone(),
                id: dsm::service_id(s.service),
                path: s.service_path.clone(),
                bytes: s.bytes,
            })
            .collect(),
        roots: answer
            .roots
            .iter()
            .map(|s| Root {
                path: s.path.clone(),
                id: dsm::service_id(s.service),
                marker: s.marker.map(Vec::from),
            })
            .collect(),
    };
    Ok(serde_json::to_vec(&cached)?)
}

pub fn decode(bytes: &[u8]) -> Option<TopAnswer> {
    let c: Cached = serde_json::from_slice(bytes).ok()?;
    let services = c
        .services
        .into_iter()
        .map(decode_service)
        .collect::<Option<_>>()?;
    let overlaps = c
        .overlaps
        .into_iter()
        .map(|s| {
            Some(PackageOverlap {
                target: s.target,
                service: dsm::service_by_id(s.id)?,
                service_path: s.path,
                bytes: s.bytes,
            })
        })
        .collect::<Option<_>>()?;
    let roots = c
        .roots
        .into_iter()
        .map(|s| {
            Some(PackageRoot {
                path: s.path,
                service: dsm::service_by_id(s.id)?,
                marker: match s.marker {
                    Some(bytes) => Some(dsm::markers().find(|m| *m == bytes)?),
                    None => None,
                },
            })
        })
        .collect::<Option<_>>()?;
    Some(TopAnswer {
        ranking: Ranking {
            target: decode_entry(&c.target)?,
            entries: c
                .entries
                .iter()
                .map(|e| decode_entry(e))
                .collect::<Option<_>>()?,
            hidden_system: c.hidden,
        },
        services: ServiceSummary {
            uses: services,
            other_snapshots_not_read: c.other_snapshots,
        },
        overlaps,
        roots,
    })
}

fn encode_service(s: &ServiceUse) -> io::Result<Service> {
    Ok(Service {
        id: s.service.map(dsm::service_id),
        path: s.path.clone(),
        paths: s.paths,
        metrics: s.metrics,
        held: s.held.0,
        entry: s.entry.as_ref().map(encode_entry).transpose()?,
        listed: s.listed.clone(),
        snapshots: s.snapshots_not_read,
        included_in_rows: s.included_in_rows,
        common_path: s.common_path.clone(),
        excluded_paths: s.excluded_paths.clone(),
        full_parts: s
            .full_parts
            .iter()
            .map(encode_service)
            .collect::<io::Result<_>>()?,
        covering_folder: s.covering_folder.clone(),
        standalone: s
            .standalone
            .as_deref()
            .map(encode_service)
            .transpose()?
            .map(Box::new),
    })
}

fn decode_service(s: Service) -> Option<ServiceUse> {
    Some(ServiceUse {
        service: match s.id {
            Some(id) => Some(dsm::service_by_id(id)?),
            None => None,
        },
        path: s.path,
        paths: s.paths,
        metrics: s.metrics,
        held: Held(s.held),
        entry: match s.entry {
            Some(bytes) => Some(decode_entry(&bytes)?),
            None => None,
        },
        listed: s.listed,
        snapshots_not_read: s.snapshots,
        included_in_rows: s.included_in_rows,
        common_path: s.common_path,
        excluded_paths: s.excluded_paths,
        full_parts: s
            .full_parts
            .into_iter()
            .map(decode_service)
            .collect::<Option<_>>()?,
        covering_folder: s.covering_folder,
        standalone: match s.standalone {
            Some(standalone) => Some(Box::new(decode_service(*standalone)?)),
            None => None,
        },
    })
}
