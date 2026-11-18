// The scan field's mode and how brightly it glows: brighter as the scan reads
// faster, from the items read per second between two progress samples.

export type FieldMode = 'gentle' | 'fast'
export type FieldDensity = 1 | 2 | 3

/** Items read per second between two progress samples; null when unknown. */
export function scanRate(before: { entries: number; at: number } | null, after: { entries: number; at: number }): number | null {
  if (!before || after.at - before.at < 500 || after.entries < before.entries) return null
  return ((after.entries - before.entries) * 1000) / (after.at - before.at)
}

/** The field glows brighter as the scan reads faster; a scan without counts sits in the middle. */
export function densityForRate(rate: number | null): FieldDensity {
  if (rate == null) return 2
  if (rate < 2_000) return 1
  return rate < 8_000 ? 2 : 3
}
