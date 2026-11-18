//! Metadata I/O admission, independent of the bounded leaf cache.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadBudget {
    pub adaptive: bool,
    pub used_bytes: u64,
    pub extent_count: u64,
    pub snapshot_count: u64,
    pub required_reads: u64,
    pub selected_reads: u64,
}

impl ReadBudget {
    pub fn select(
        requested: u64,
        used_bytes: u64,
        extent_count: u64,
        snapshot_count: u64,
        required_reads: u64,
    ) -> Self {
        let adaptive = requested == super::leaf_ranges::DEFAULT_READ_BUDGET;
        // Bytes account for large allocations; extent count accounts for
        // fragmentation. Sharing adds logarithmic headroom, never N complete
        // passes for N snapshots. All arithmetic saturates at the u64 boundary.
        let sharing = (64 - snapshot_count.leading_zeros()).min(8);
        let by_extents = (u128::from(extent_count) * u128::from(4 + sharing))
            .div_ceil(4)
            .min(u128::from(u64::MAX)) as u64;
        let selected_reads = if adaptive {
            16_384
                .max(used_bytes.div_ceil(8 << 20))
                .max(by_extents)
                .max(required_reads)
        } else {
            requested
        };
        Self {
            adaptive,
            used_bytes,
            extent_count,
            snapshot_count,
            required_reads,
            selected_reads,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::leaf_ranges::DEFAULT_READ_BUDGET;

    #[test]
    fn small_volumes_spend_less_and_large_fragmented_snapshot_volumes_scale() {
        let small = ReadBudget::select(DEFAULT_READ_BUDGET, 1 << 30, 2_000, 0, 0);
        let large = ReadBudget::select(DEFAULT_READ_BUDGET, 500 << 40, 64_000_000, 4096, 0);
        assert!(small.selected_reads < 500_000);
        assert!(large.selected_reads > 64_000_000);
        // Snapshot count must not multiply the whole scan by thousands.
        assert!(large.selected_reads < 64_000_000 * 8);
    }

    #[test]
    fn deep_admission_covers_observed_reference_work_and_manual_limits_stay_exact() {
        let plan = ReadBudget::select(DEFAULT_READ_BUDGET, 1 << 30, 100, 1, 9_000_000);
        assert!(plan.selected_reads >= 9_000_000);
        assert_eq!(
            ReadBudget::select(0, u64::MAX, u64::MAX, u64::MAX, 42).selected_reads,
            0
        );
        assert_eq!(
            ReadBudget::select(73, 1 << 40, 1_000_000, 100, 1_000).selected_reads,
            73
        );
    }

    #[test]
    fn proportional_volumes_have_linear_admission_and_saturate_without_wrapping() {
        let a = ReadBudget::select(DEFAULT_READ_BUDGET, 250 << 40, 32_000_000, 4096, 0);
        let b = ReadBudget::select(DEFAULT_READ_BUDGET, 500 << 40, 64_000_000, 4096, 0);
        assert!(b.selected_reads >= a.selected_reads * 2 - 1);
        assert!(b.selected_reads <= a.selected_reads * 2 + 1);
        assert_eq!(
            ReadBudget::select(DEFAULT_READ_BUDGET, u64::MAX, u64::MAX, u64::MAX, u64::MAX)
                .selected_reads,
            u64::MAX
        );
    }
}
