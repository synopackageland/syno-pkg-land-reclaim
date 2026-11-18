//! The package-managed data (SPEC 5.7) a ranked or simulated path is,
//! lies inside or holds: what `top` and `simulate-delete` hint at, so
//! the space is freed in the package and never by deleting its files.

use crate::document::PackageDataJson;
use crate::dsm::Service;
use crate::summary::PackageRoot;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Package data found at, above or below a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageData {
    pub service: &'static Service,
    /// The marker entry, when a marker made a folder package data.
    pub marker: Option<&'static [u8]>,
    /// The path holds the data below itself rather than being in it.
    pub holds: bool,
}

/// The package data `path`, below `mount`, is or lies inside, by name or
/// through a marker in a folder above it (checked live), else the first
/// of `roots` below it.
pub fn lookup(path: &[u8], mount: &[u8], roots: &[PackageRoot]) -> Option<PackageData> {
    let within = |service, marker| PackageData {
        service,
        marker,
        holds: false,
    };
    if let Some(service) = crate::dsm::package_data_of(path, mount) {
        return Some(within(service, None));
    }
    let marker = crate::guard::marker_above(
        Path::new(OsStr::from_bytes(path)),
        Path::new(OsStr::from_bytes(mount)),
    );
    if let Ok(Some(marker)) = marker {
        let service = crate::dsm::marked_by(marker).expect("a marker names its service");
        return Some(within(service, Some(marker)));
    }
    roots
        .iter()
        .find(|root| {
            root.path
                .strip_prefix(path)
                .is_some_and(|rest| rest.starts_with(b"/"))
        })
        .map(|root| PackageData {
            service: root.service,
            marker: root.marker,
            holds: true,
        })
}

impl From<&PackageData> for PackageDataJson {
    fn from(data: &PackageData) -> Self {
        let service = data.service;
        let kind = data
            .marker
            .and_then(crate::dsm::marker_refusal)
            .unwrap_or_else(|| service.refusal());
        PackageDataJson {
            service: service.code,
            package: service.package,
            manage_in: service.manage_in,
            manage_in_id: service.manage_in_id(),
            confirmed: service.confirmed,
            marker: data.marker.map(|m| String::from_utf8_lossy(m).into_owned()),
            holds: data.holds,
            message: if data.holds {
                format!("holds {kind}")
            } else {
                kind
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spill::testing::scratch;

    fn bytes(path: &std::path::Path) -> &[u8] {
        path.as_os_str().as_bytes()
    }

    fn code(found: Option<PackageData>) -> Option<(&'static str, bool, bool)> {
        found.map(|p| (p.service.code, p.marker.is_some(), p.holds))
    }

    #[test]
    fn a_guarded_service_path_or_its_contents_is_package_data_by_name() {
        let found = lookup(b"/v/share/@ActiveBackup/task", b"/v", &[]);
        assert_eq!(code(found), Some(("active_backup_data", false, false)));
        assert_eq!(code(lookup(b"/v/share/#recycle/a", b"/v", &[])), None);
    }

    #[test]
    fn a_path_inside_a_folder_holding_a_marker_is_package_data() {
        let volume = scratch("package-marked");
        std::fs::create_dir_all(volume.join("share/@ActiveBackup")).unwrap();
        std::fs::create_dir_all(volume.join("share/ActiveBackup_2026/v1")).unwrap();
        for path in ["share", "share/ActiveBackup_2026/v1"] {
            let found = lookup(bytes(&volume.join(path)), bytes(&volume), &[]);
            assert_eq!(
                code(found),
                Some(("active_backup_data", true, false)),
                "{path}"
            );
        }
        std::fs::create_dir(volume.join("@ActiveBackup")).unwrap();
        std::fs::create_dir(volume.join("other")).unwrap();
        assert_eq!(
            code(lookup(bytes(&volume.join("other")), bytes(&volume), &[])),
            None
        );
    }

    #[test]
    fn a_path_holding_a_package_root_below_it_holds_package_data() {
        let hbk = crate::dsm::package_data(b"x.hbk").unwrap();
        let roots = [PackageRoot {
            path: b"/v/docs/backups/x.hbk".to_vec(),
            service: hbk,
            marker: None,
        }];
        let found = lookup(b"/v/docs", b"/v", &roots);
        assert_eq!(code(found), Some(("hyper_backup", false, true)));
        assert_eq!(code(lookup(b"/v/doc", b"/v", &roots)), None);
        assert_eq!(code(lookup(b"/v/music", b"/v", &roots)), None);
    }

    #[test]
    fn the_json_says_who_manages_the_data_and_whether_the_path_holds_it() {
        let abb = crate::dsm::marked_by(b"@ActiveBackup").unwrap();
        let share = PackageData {
            service: abb,
            marker: Some(b"@ActiveBackup"),
            holds: false,
        };
        let json = PackageDataJson::from(&share);
        assert_eq!(json.service, "active_backup_data");
        assert_eq!(json.marker.as_deref(), Some("@ActiveBackup"));
        assert_eq!(
            json.message,
            crate::dsm::marker_refusal(b"@ActiveBackup").unwrap()
        );
        assert!(
            json.message
                .contains("manage it in Active Backup for Business")
        );
        let hbk = crate::dsm::package_data(b"x.hbk").unwrap();
        let folder = PackageData {
            service: hbk,
            marker: None,
            holds: true,
        };
        let json = PackageDataJson::from(&folder);
        assert_eq!(json.message, format!("holds {}", hbk.refusal()));
        assert!(json.holds && json.marker.is_none());
    }
}
