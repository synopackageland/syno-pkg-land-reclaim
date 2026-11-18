// The tone of a scan's reading wherever a badge shows it: a stable reading is
// success, a changed one information, a partial one a warning.
export type ConsistencyTone = 'success' | 'info' | 'warning' | 'neutral'

const TONES: Record<string, ConsistencyTone> = { 'observed-stable': 'success', 'changed-during-scan': 'info', partial: 'warning' }

export function consistencyTone(reading: string | null | undefined): ConsistencyTone {
  return TONES[reading ?? ''] ?? 'neutral'
}
