// Synthetic display fixtures: identical data with small unresolved refs or a
// attribution-only unknown above the 3% threshold, a zero-byte namespace gap, or dangling entries from filesystem churn (one: muted line; eleven: banner). Normal source fixtures retain their original values.
import type { Attribution } from '@/components/reportCompleteness'

export type AttributionMode = 'small' | 'large' | 'gap' | 'churn' | 'churn_many'
const UNKNOWN_BYTES: Record<AttributionMode, number> = { small: 128 * 2 ** 20, large: 5 * 2 ** 30, gap: 0, churn: 4096, churn_many: 4096 }
const DANGLING: Partial<Record<AttributionMode, number>> = { churn: 1, churn_many: 11 }

export function attributionFixture(document: unknown, mode: AttributionMode) {
  const d = structuredClone(document) as Record<string, unknown>
  const dangling = DANGLING[mode] ?? 0
  const gap = mode === 'gap' || dangling > 0
  const attribution: Attribution = { unknown_bytes: UNKNOWN_BYTES[mode], used_bytes: 100 * 2 ** 30, coverage_gaps: gap }
  if (dangling) attribution.namespace_churn = { dangling_entries: dangling }
  const visit = (value: unknown) => {
    if (!value || typeof value !== 'object') return
    if (Array.isArray(value)) { value.forEach(visit); return }
    const record = value as Record<string, unknown>
    if ('consistency' in record && 'coverage' in record) {
      record.consistency = 'partial'
      record.attribution = { ...attribution }
      const coverage = record.coverage as Record<string, unknown>
      coverage.namespace_uncertain_subvolumes = gap ? [256] : []
      for (const key of ['encrypted_directories', 'unlinked_subvolume_entries', 'dangling_entries', 'cycles', 'unsupported_locations', 'missing_subvolume_roots']) coverage[key] = 0
      coverage.dangling_entries = dangling
      coverage.reference_accounting = mode === 'gap' ? 'unavailable' : 'partial'
    }
    if ('last_consistency' in record) {
      record.last_consistency = 'partial'
      record.last_attribution = { ...attribution }
    }
    if ('outcome' in record && 'df_used_bytes' in record) {
      record.outcome = 'partial'
      record.attribution = { ...attribution }
    }
    for (const [key, child] of Object.entries(record)) if (key !== 'attribution' && key !== 'last_attribution') visit(child)
  }
  visit(d)
  if ('completion' in d) d.completion = 'partial'
  if ('results' in d && Array.isArray(d.results)) {
    for (const result of d.results as Record<string, unknown>[]) {
      if (!Array.isArray(result.parts)) continue
      const parts = result.parts as Record<string, unknown>[]
      const unknown = parts.find(part => part.kind === 'unknown')
      if (!unknown) continue
      const before = Number(unknown.bytes)
      unknown.bytes = attribution.unknown_bytes
      unknown.reasons = [{ code: mode === 'gap' ? 'coverage_gap' : 'shared_tree_owner_unresolved', bytes: attribution.unknown_bytes }]
      const other = parts.find(part => part.kind === 'system_other')
      if (other) other.bytes = Number(other.bytes) + before - attribution.unknown_bytes
      else parts.push({ kind: 'system_other', bytes: before - attribution.unknown_bytes, reclaimable_bytes: null, held_bytes: null, items: [] })
      const report = result.report as Record<string, unknown>
      if (report) report.attribution = { ...attribution, used_bytes: (result.space as { df: { used_bytes: number } }).df.used_bytes }
    }
  }
  return d
}
