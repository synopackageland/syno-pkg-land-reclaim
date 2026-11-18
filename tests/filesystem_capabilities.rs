use syno_reclaim::filesystem::FilesystemKind;
use syno_reclaim::mounts::{parse_mountinfo, supported_volumes, unsupported_volumes};

#[test]
fn discovers_ext4_data_volumes_without_exposing_the_system_partition_or_aliases() {
    let mounts = parse_mountinfo(b"1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 8:1 /home /home rw - ext4 /dev/sda1 rw\n3 1 8:2 / /volume1 rw - ext4 /dev/sda2 rw\n4 3 8:2 /share /volume1/alias rw - ext4 /dev/sda2 rw\n5 1 0:5 / /volume2 rw - btrfs /dev/sdb rw\n6 1 8:3 / /volume3 rw - xfs /dev/sdc rw\n");
    let volumes = supported_volumes(&mounts);
    assert_eq!(volumes.len(), 2);
    assert_eq!(volumes[0].primary.mount_point.to_str(), Some("/volume1"));
    assert_eq!(volumes[0].aliases, 1);
    assert_eq!(unsupported_volumes(&mounts).len(), 1);
}

#[test]
fn ext4_reports_its_real_capabilities_and_moves_items_to_the_recycle_bin() {
    let capabilities = FilesystemKind::Ext4.capabilities();
    assert!(capabilities.scan);
    assert!(!capabilities.snapshots);
    assert!(!capabilities.reflinks);
    // Cleanup moves items into the shared folder's Recycle Bin with DSM's
    // own tool, on ext4 as on Btrfs.
    assert!(capabilities.cleanup);
    assert!(capabilities.cleanup_unavailable_reason.is_none());
    assert_eq!(serde_json::to_value(FilesystemKind::Ext4).unwrap(), "ext4");
    assert!(FilesystemKind::Btrfs.capabilities().snapshots);
}

#[cfg(target_os = "linux")]
#[test]
fn ext4_identity_checks_the_superblock_magic_and_rejects_empty_uuids() {
    let mut superblock = [0; 120];
    assert!(syno_reclaim::ext4::superblock_uuid(&superblock).is_err());
    superblock[56..58].copy_from_slice(&0xef53_u16.to_le_bytes());
    assert!(syno_reclaim::ext4::superblock_uuid(&superblock).is_err());
    superblock[104..120].copy_from_slice(&[7; 16]);
    assert_eq!(
        syno_reclaim::ext4::superblock_uuid(&superblock).unwrap(),
        [7; 16]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn trusted_share_inventory_is_counted_and_never_treated_as_arbitrary_paths() {
    use syno_reclaim::dsm_shares::parse_inventory;
    assert_eq!(
        parse_inventory(b"2 Listed:\nshare\nbytes-\xff\n")
            .unwrap()
            .len(),
        2
    );
    assert!(parse_inventory(b"0 Listed:\n").unwrap().is_empty());
    assert!(parse_inventory(b"1 Listed:\n../outside\n").is_err());
    assert!(parse_inventory(b"2 Listed:\nshare\n").is_err());
    assert!(parse_inventory(b"2 Listed:\nshare\nshare\n").is_err());
}
