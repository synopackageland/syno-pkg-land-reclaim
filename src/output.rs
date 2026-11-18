//! Output primitives (SPEC 14, 22.7): byte-safe paths for humans and
//! JSON, and human-readable sizes.

use crate::model::tree::Metrics;
use serde::{Deserialize, Serialize};

/// A path for humans: control characters, backslashes and invalid
/// UTF-8 are escaped so names cannot inject terminal sequences.
pub fn escape_human(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => {
                    for b in c.to_string().bytes() {
                        out.push_str(&format!("\\x{b:02x}"));
                    }
                }
                c => out.push(c),
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

/// A path or name in JSON: display text, plus lossless hex when the
/// bytes are not UTF-8. The display text is never used to find files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonPath {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_hex: Option<String>,
}

impl JsonPath {
    pub fn new(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(text) => JsonPath {
                path: text.to_string(),
                path_hex: None,
            },
            Err(_) => JsonPath {
                path: String::from_utf8_lossy(bytes).into_owned(),
                path_hex: Some(crate::report::hex_bytes::encode(bytes)),
            },
        }
    }

    /// The original bytes.
    pub fn bytes(&self) -> Vec<u8> {
        match &self.path_hex {
            Some(hex) => crate::report::hex_bytes::decode(hex).unwrap_or_default(),
            None => self.path.as_bytes().to_vec(),
        }
    }
}

/// A size as DSM's Storage Manager shows it (the UI's formatFileSize): in
/// 1024s labelled KB/MB/GB/TB, at most one decimal: `512 B`, `4 KB`,
/// `874.6 GB`, `1.8 TB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let tenths = format!("{value:.1}");
    let shown = tenths.strip_suffix(".0").unwrap_or(&tenths);
    format!("{shown} {}", UNITS[unit])
}

/// A short count: `999`, `50.8k`, `5.51M`.
pub fn human_count(n: u64) -> String {
    const UNITS: [(f64, &str); 3] = [(1e9, "G"), (1e6, "M"), (1e3, "k")];
    let Some(&(scale, unit)) = UNITS.iter().find(|(scale, _)| n as f64 >= *scale) else {
        return n.to_string();
    };
    let value = n as f64 / scale;
    match value {
        v if v < 10.0 => format!("{v:.2}{unit}"),
        v if v < 100.0 => format!("{v:.1}{unit}"),
        v => format!("{v:.0}{unit}"),
    }
}

/// Metrics in JSON. Reclaimable bytes are exact only when no reference
/// is unresolved, and `null` otherwise: unknown, never zero (SPEC 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JsonMetrics {
    pub logical: u64,
    pub physical: u64,
    pub shared: u64,
    pub reclaimable: Option<u64>,
    /// The conservative resolved part of `reclaimable`.
    pub reclaimable_resolved: u64,
    pub unknown: u64,
    pub pinned_unreferenced: Option<u64>,
    pub extents: Option<u64>,
    pub inline: Option<u64>,
    pub entries: u64,
    pub files: u64,
}

impl From<&Metrics> for JsonMetrics {
    fn from(m: &Metrics) -> Self {
        JsonMetrics {
            logical: m.logical,
            physical: m.physical,
            shared: m.shared(),
            reclaimable: (m.unknown == 0).then_some(m.reclaimable),
            reclaimable_resolved: m.reclaimable,
            unknown: m.unknown,
            pinned_unreferenced: Some(m.pinned),
            extents: Some(m.extents),
            inline: Some(m.inline),
            entries: m.entries,
            files: m.files,
        }
    }
}

impl JsonMetrics {
    pub fn with_filesystem(mut self, filesystem: crate::filesystem::FilesystemKind) -> Self {
        if filesystem == crate::filesystem::FilesystemKind::Ext4 {
            self.pinned_unreferenced = None;
            self.extents = None;
            self.inline = None;
        }
        self
    }
}

/// A stable symbolic code with a human message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Notice {
    pub code: &'static str,
    pub message: String,
}

impl Notice {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Notice {
            code,
            message: message.into(),
        }
    }
}

/// A per-target failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TargetError {
    #[serde(flatten)]
    pub target: JsonPath,
    pub code: &'static str,
    pub message: String,
    /// Who holds the writer lock, for `busy`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<Box<crate::state::LockHolder>>,
    /// The limiting resource when a rootfs destination is full.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<Box<&'static str>>,
    /// Why a `protected_target` is protected (boxed: errors stay small).
    #[serde(flatten)]
    pub protection: Option<Box<Protection>>,
    /// What a duplicates selection says about the target (boxed: errors
    /// stay small).
    #[serde(flatten)]
    pub duplicate: Option<Box<DuplicateTarget>>,
}

/// A target of a duplicates selection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DuplicateTarget {
    /// The selected duplicate group whose copy the target is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// For `keeper_conflict`: the groups that disagree on the copy at
    /// `path`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<KeeperConflict>,
}

/// A copy the user kept in `kept_in` that removing `removed_by` removes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeeperConflict {
    pub kept_in: GroupName,
    pub removed_by: GroupName,
}

/// A duplicate group by id and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupName {
    pub id: String,
    pub name: JsonPath,
}

/// Why a target is protected: `reason` is one of
/// [`crate::guard::PROTECTED_REASONS`]; `package` names the package whose
/// data it is, for reason `package`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Protection {
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<&'static str>,
}

impl TargetError {
    pub fn new(target: JsonPath, code: &'static str, message: impl Into<String>) -> Self {
        TargetError {
            target,
            code,
            message: message.into(),
            holder: None,
            limit: None,
            protection: None,
            duplicate: None,
        }
    }

    /// Says why the target is protected.
    pub fn protected(mut self, reason: &'static str, package: Option<&'static str>) -> Self {
        self.protection = Some(Box::new(Protection { reason, package }));
        self
    }

    pub fn reason(&self) -> Option<&'static str> {
        self.protection.as_ref().map(|p| p.reason)
    }
}

/// Fixed-width columns, left-aligned first column.
pub fn table(header: &[&str], rows: &[Vec<String>]) -> String {
    let aligns: Vec<Align> = (0..header.len())
        .map(|i| if i == 0 { Align::Left } else { Align::Right })
        .collect();
    aligned_table(header, &aligns, rows)
}

/// How a table column lines up its cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A table whose columns line up as `aligns` says, one line per row.
pub fn aligned_table(header: &[&str], aligns: &[Align], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    let mut line = |cells: Vec<&str>| {
        let mut text = String::new();
        for (i, ((cell, w), align)) in cells.iter().zip(&widths).zip(aligns).enumerate() {
            if i > 0 {
                text.push_str("  ");
            }
            match align {
                Align::Left => text.push_str(&format!("{cell:<w$}")),
                Align::Right => text.push_str(&format!("{cell:>w$}")),
            }
        }
        out.push_str(text.trim_end());
        out.push('\n');
    };
    line(header.to_vec());
    for row in rows {
        line(row.iter().map(String::as_str).collect());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_controls_backslashes_and_invalid_utf8() {
        assert_eq!(escape_human(b"/v/Movies \xc3\xa9"), "/v/Movies é");
        assert_eq!(escape_human(b"/v/a\x1b[31mb"), "/v/a\\x1b[31mb");
        assert_eq!(escape_human(b"/v/a\nb\\c\td"), "/v/a\\nb\\\\c\\td");
        assert_eq!(escape_human(b"/v/\xff\xfe"), "/v/\\xff\\xfe");
        assert_eq!(escape_human("/v/\u{9b}x".as_bytes()), "/v/\\xc2\\x9bx");
    }

    #[test]
    fn json_paths_add_hex_only_for_non_utf8() {
        let plain = serde_json::to_value(JsonPath::new(b"/v/a")).unwrap();
        assert_eq!(plain, serde_json::json!({"path": "/v/a"}));
        let raw = serde_json::to_value(JsonPath::new(b"/v/\xff")).unwrap();
        assert_eq!(
            raw,
            serde_json::json!({"path": "/v/\u{fffd}", "path_hex": "2f762fff"})
        );
    }

    /// As DSM's Storage Manager shows sizes (the UI's formatFileSize):
    /// a test NAS's volume1 is 874.6 GB there, not 939.1 GB.
    #[test]
    fn sizes_are_shown_as_storage_manager_shows_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1 KB");
        assert_eq!(human_bytes(4096), "4 KB");
        assert_eq!(human_bytes(1_572_864), "1.5 MB");
        assert_eq!(human_bytes(939_094_599_270), "874.6 GB");
        assert_eq!(human_bytes(420 << 30), "420 GB");
        assert_eq!(human_bytes((18 << 40) / 10), "1.8 TB");
        assert_eq!(human_bytes(1 << 60), "1048576 TB");
    }

    #[test]
    fn counts_keep_three_significant_digits() {
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(50_838), "50.8k");
        assert_eq!(human_count(5_510_000), "5.51M");
        assert_eq!(human_count(48_100_000), "48.1M");
        assert_eq!(human_count(2_000_000_000), "2.00G");
    }

    #[test]
    fn reclaimable_is_exact_only_without_unknown_bytes() {
        let resolved = Metrics {
            logical: 12,
            physical: 10,
            reclaimable: 6,
            pinned: 1,
            ..Metrics::default()
        };
        let json = serde_json::to_value(JsonMetrics::from(&resolved)).unwrap();
        assert_eq!(json["reclaimable"], 6);
        assert_eq!(json["reclaimable_resolved"], 6);
        assert_eq!(json["shared"], 4);
        assert_eq!(json["unknown"], 0);
        assert_eq!(json["pinned_unreferenced"], 1);
        let unresolved = Metrics {
            unknown: 3,
            ..resolved
        };
        let json = serde_json::to_value(JsonMetrics::from(&unresolved)).unwrap();
        assert_eq!(json["reclaimable"], serde_json::Value::Null);
        assert_eq!(json["reclaimable_resolved"], 6);
        assert_eq!(json["shared"], 1);
        assert_eq!(json["unknown"], 3);
    }

    #[test]
    fn tables_align_columns() {
        let rows = vec![
            vec!["/v/a".to_string(), "1.0 MiB".to_string()],
            vec!["/v/longer".to_string(), "12 GiB".to_string()],
        ];
        assert_eq!(
            table(&["PATH", "SIZE"], &rows),
            "PATH          SIZE\n/v/a       1.0 MiB\n/v/longer   12 GiB\n"
        );
    }
}
