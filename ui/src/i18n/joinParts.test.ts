import { describe, expect, it } from 'vitest'
import { joinParts, joinSentences } from './joinParts'
import { catalogs } from './catalogs'
import { createTranslator } from './t'

const en = [
  'volume1: Usage has increased by 50 GB since the last scan. Results may have changed.',
  'volume2: Usage has decreased by 60 GB since the last scan. Results may have changed.',
]
const zhTW = [
  'volume1：使用量較上次掃描增加 50 GB，結果可能已變動。',
  'volume2：使用量較上次掃描減少 60 GB，結果可能已變動。',
]
const zhCN = [
  'volume1：使用量较上次扫描增加 50 GB，结果可能已变化。',
  'volume2：使用量较上次扫描减少 60 GB，结果可能已变化。',
]
const ja = [
  'volume1：使用量が前回のスキャンから 50 GB 増加しました。結果が変わっている可能性があります。',
  'volume2：使用量が前回のスキャンから 60 GB 減少しました。結果が変わっている可能性があります。',
]

describe('joinParts', () => {
  it('returns the single part and the empty list unchanged', () => {
    expect(joinParts([], 'en')).toBe('')
    expect(joinParts(['Only one'], 'zh-TW')).toBe('Only one')
  })

  it.each(['en', 'zh-TW', 'ja'] as const)('%s puts a separator only between two parts that both have content', locale => {
    expect(joinParts(['Shared folder', ''], locale)).toBe('Shared folder')
    expect(joinParts(['', 'Shared folder'], locale)).toBe('Shared folder')
    expect(joinParts(['VM disk', '  ', '7d4e9b20'], locale)).toBe('VM disk · 7d4e9b20')
    expect(joinParts(['', ' '], locale)).toBe('')
    expect(joinParts(['Done.', '', 'Next'], locale)).toBe(locale === 'en' ? 'Done. Next' : 'Done.Next')
  })

  it.each(['en', 'de', 'fr', 'es', 'it', 'ko'] as const)('%s keeps " · " between labels and a space between sentences', locale => {
    expect(joinParts(['Recycle Bin', 'Encrypted'], locale)).toBe('Recycle Bin · Encrypted')
    expect(joinParts(['Done.', 'Next'], locale)).toBe('Done. Next')
  })

  it.each(['zh-TW', 'zh-CN', 'ja'] as const)('%s concatenates sentences after 。！？ both full and half width', locale => {
    const labels = ['資源回收筒', '已加密']
    expect(joinParts(labels, locale)).toBe('資源回收筒 · 已加密')
    expect(joinParts(['第一句。', '第二句。'], locale)).toBe('第一句。第二句。')
    expect(joinParts(['第一句！', '第二句？'], locale)).toBe('第一句！第二句？')
  })

  it.each(['en', 'ko'] as const)('%s never puts the middle dot right after a full stop', locale => {
    const joined = joinParts(['Done.', 'Next'], locale)
    expect(joined).toBe('Done. Next')
    expect(joined).not.toContain('. ·')
  })

  it('merges the usage-change summary without double punctuation in every shipped locale', () => {
    expect(joinParts(en, 'en')).toBe(`${en[0]} ${en[1]}`)
    expect(joinParts(en, 'en')).not.toContain('. ·')
    expect(joinParts(zhTW, 'zh-TW')).toBe(`${zhTW[0]}${zhTW[1]}`)
    expect(joinParts(zhTW, 'zh-TW')).not.toContain('。 ·')
    expect(joinParts(zhCN, 'zh-CN')).toBe(`${zhCN[0]}${zhCN[1]}`)
    expect(joinParts(ja, 'ja')).toBe(`${ja[0]}${ja[1]}`)
    expect(joinParts(ja, 'ja')).not.toContain('。 ·')
  })

  it('treats full-width ！？ and an ellipsis as sentence ends in Latin locales too', () => {
    expect(joinParts(['Done！', 'Next'], 'en')).toBe('Done！ Next')
    expect(joinParts(['Done…', 'Next'], 'en')).toBe('Done… Next')
  })
})

it.each(['zh-TW', 'zh-CN', 'ja'] as const)('%s separates an included-row label from its context', locale => {
  const { t } = createTranslator(locale, catalogs)
  const hint = t('explore.row.includedIn', { folder: 'backup' })
  const context = t('explore.flag.recycle')
  expect(hint).toContain('backup')
  expect(joinParts([hint, context], locale)).toBe(hint + ' · ' + context)
  expect(joinParts([hint, context], locale)).not.toContain('。 ·')
})

// A period separating sentences is part of the paragraph, even when its last
// sentence has no final punctuation. Numeric metadata belongs on another line.
it.each(['en', 'zh-TW', 'zh-CN', 'ja', 'ko', 'de', 'fr', 'es', 'it'] as const)('%s separates composed cleanup sentences and keeps only the paragraph ending unpunctuated', locale => {
  const { t } = createTranslator(locale, catalogs)
  for (const tool of ['logrotate', 'delete_journal', 'delete_log']) {
    const impact = t(`rootfsClean.toolImpact.${tool}`)
    const estimate = t('rootfsClean.mode.toolEstimate')
    const joined = joinSentences([impact, estimate], locale)
    const boundary = ['zh-TW', 'zh-CN', 'ja'].includes(locale) ? '。' : '. '
    expect(joined).toBe(impact + boundary + estimate)
    expect(joined).not.toMatch(/[.。!?！？]$/)
  }
  expect(joinSentences(['First sentence. Second sentence.', 'Last sentence.'], locale))
    .toContain('First sentence. Second sentence.')
})
