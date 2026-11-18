import { createFormatters, formatVolume } from './format'
import { createTranslator } from './t'

const catalogs = {
  en: {
    'common.unknown': 'unknown',
  },
}
const f = createFormatters(createTranslator('en', catalogs))

describe('formatters', () => {
  // Storage Manager counts in 1024s and labels them KB/MB/GB/TB: a test NAS's
  // volume1 is 874.6 GB there, not 939.1 GB.
  it('formats bytes in 1024s with the units Storage Manager shows', () => {
    expect(f.formatBytes(0)).toBe('0 B')
    expect(f.formatBytes(1023)).toBe('1023 B')
    expect(f.formatBytes(1024)).toBe('1 KB')
    expect(f.formatBytes(1_572_864)).toBe('1.5 MB')
    expect(f.formatBytes(939_094_599_270)).toBe('874.6 GB')
    expect(f.formatBytes(3_529_324_134_400)).toBe('3.2 TB')
    // In every language, as DSM shows it.
    expect(createFormatters(createTranslator('de', {})).formatBytes(1_572_864)).toBe('1.5 MB')
  })

  it('renders null as unknown, never 0', () => {
    expect(f.formatBytes(null)).toBe('unknown')
    expect(f.formatPercent(null)).toBe('unknown')
    expect(f.formatDuration(null)).toBe('unknown')
    expect(f.formatDateTime(null)).toBe('unknown')
    expect(f.formatCompactDateTime(null)).toBe('unknown')
    expect(f.formatRelative(null)).toBe('unknown')
    expect(f.formatCount(null)).toBe('unknown')
  })

  it('formats percent and counts', () => {
    expect(f.formatPercent(0.1784)).toBe('17.8%')
    expect(f.formatCount(3120442)).toBe('3,120,442')
  })

  // The library's formatters (UI 0.9.5): hours and minutes in the locale's own words.
  it('formats durations as m:ss below an hour, else hours and minutes', () => {
    expect(f.formatDuration(65)).toBe('1:05')
    expect(f.formatDuration(0.12)).toBe('<0:01')
    expect(f.formatDuration(0)).toBe('0:00')
    expect(f.formatDuration(3 * 3600 + 7 * 60 + 5)).toBe('3 hr 7 min')
    expect(createFormatters(createTranslator('zh-TW', {})).formatDuration(3 * 3600 + 7 * 60)).toBe('3 小時 7 分鐘')
  })

  it('formats dates and relative times', () => {
    expect(f.formatDateTime('2026-09-30T06:32:00Z')).toMatch(/2026/)
    const now = Date.parse('2026-09-30T12:00:00Z')
    expect(f.formatRelative('2026-09-27T12:00:00Z', now)).toBe('3 days ago')
    expect(f.formatRelative('2026-09-30T11:58:00Z', now)).toBe('2 minutes ago')
  })

  it('uses compact table dates in the DSM locale, with a year for older dates', () => {
    const oldDate = '2024-12-27T11:48:00'
    expect(f.formatCompactDateTime(oldDate)).toBe('Dec 27, 2024')
    expect(createFormatters(createTranslator('zh-TW', {})).formatCompactDateTime(oldDate)).toBe('2024/12/27')
  })
})

describe('formatVolume', () => {
  it('names a volume by its mount folder, as Storage Manager lists it', () => {
    expect(formatVolume('/volume1')).toBe('volume1')
    expect(formatVolume('/volumeUSB1/usbshare')).toBe('usbshare')
  })
})
