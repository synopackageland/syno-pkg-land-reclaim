/** Presentation only. Stored completion and cleanup certification stay authoritative. */
/** `namespace_churn`: the backend found dangling entries in trees that changed during the scan, and no other gap. */
export type Attribution = {
  unknown_bytes: number
  used_bytes: number | null
  coverage_gaps: boolean
  namespace_churn?: { dangling_entries: number } | null
  approximation?: { changed_subvolumes: number; skipped_roots: number; unaccounted_bytes: number } | null
}
export type DisplayReport = { consistency?: string | null; attribution?: Attribution | null }
const GiB = 2 ** 30
const SMALL_SHARE = 0.03
const MINOR_CHURN_MAX_DANGLING = 10
const ABORTED = new Set(['failed', 'cancelled', 'paused', 'runtime_limit', 'resource_limit', 'interrupted'])

/** The single threshold used by banners, notes, report chips, cards and history badges. */
export function reportCompleteness(report?: DisplayReport | null, completion?: string | null, hasErrors = false) {
  const a = report?.attribution
  const partial = completion === 'partial' || report?.consistency === 'partial'
  const interrupted = hasErrors || ABORTED.has(completion ?? '')
  const measurable = !!a && Number.isFinite(a.unknown_bytes) && a.unknown_bytes >= 0
    && a.used_bytes != null && Number.isFinite(a.used_bytes) && a.used_bytes > 0
  const small = measurable && a!.unknown_bytes <= GiB && a!.unknown_bytes <= a!.used_bytes! * SMALL_SHARE
  const dangling = a?.namespace_churn?.dangling_entries
  const minorChurn = Number.isInteger(dangling) && dangling! >= 1 && dangling! <= MINOR_CHURN_MAX_DANGLING
  const approximate = !!a?.approximation
  const warn = interrupted || (!approximate && ((!!a?.coverage_gaps && !minorChurn) || (a ? !measurable && (partial || a.unknown_bytes > 0) : partial)))
  const unattributedOnly = !warn && (approximate || (measurable && !small && a!.unknown_bytes > 0))
  const churnEntries = minorChurn && !warn ? dangling! : 0
  return { warn, churnEntries, smallUnknown: small && a!.unknown_bytes > 0 && !warn, unknownNote: unattributedOnly, unknownBytes: a?.unknown_bytes ?? 0 }
}

/** An honest neutral label replaces partial; never claims an atomic or complete scan. */
export function visibleConsistency(report?: DisplayReport | null) {
  const { warn, unknownNote, churnEntries } = reportCompleteness(report)
  if (report?.attribution?.approximation && !warn) return 'approximate'
  if (report?.attribution && warn) return 'partial'
  if (report?.consistency !== 'partial') return report?.consistency ?? null
  if (warn) return 'partial'
  if (churnEntries) return 'changed-during-scan'
  return unknownNote ? 'unattributed' : 'small_unknown'
}
