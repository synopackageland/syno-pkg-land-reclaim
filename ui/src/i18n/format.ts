// Formatters: the library's, which size exactly as
// DSM's Storage Manager shows (1024s labelled KB/MB/GB/TB) and use Intl for
// the rest, and null is always "unknown", never 0.
import { formatDateTime, formatDuration, formatFileSize, formatPercent, formatRelativeTime } from '@synopackageland/ui'
import type { Translator } from './t'

/** A volume's display name: its mount folder, e.g. `/volume1` → `volume1`. */
export function formatVolume(mount: string) {
  return mount.slice(mount.lastIndexOf('/') + 1) || mount
}

export function createFormatters({ locale, t }: Translator) {
  const unknown = () => t('common.unknown')
  const options = () => ({ locale, unknown: unknown() })
  const count = new Intl.NumberFormat(locale)
  const dateTime = new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' })

  return {
    formatBytes: (bytes: number | null | undefined) => formatFileSize(bytes, options()),
    formatCount: (value: number | null | undefined) => (value == null ? unknown() : count.format(value)),
    formatPercent: (ratio: number | null | undefined) => formatPercent(ratio, options()),
    /** Seconds → `m:ss` below one hour (`<0:01` under a second), else hours and minutes. */
    formatDuration: (seconds: number | null | undefined) => formatDuration(seconds, options()),
    /** RFC 3339 → local date and time. */
    formatDateTime: (value: string | null | undefined) => (value == null ? unknown() : dateTime.format(new Date(value))),
    /** The UI's one-line table date, in this DSM window's language. */
    formatCompactDateTime: (value: string | null | undefined) => formatDateTime(value, { ...options(), style: 'compact' }),
    formatRelative: (value: string | null | undefined, now = Date.now()) =>
      value == null ? unknown() : formatRelativeTime(value, { ...options(), now }),
  }
}

export type Formatters = ReturnType<typeof createFormatters>
