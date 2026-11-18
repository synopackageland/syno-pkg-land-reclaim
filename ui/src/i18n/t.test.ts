import { createTranslator, detectLocale } from './t'

const catalogs = {
  en: {
    'common.unknown': 'unknown',
    'code.unknown': 'Unrecognised status ({code})',
    'code.state.running': 'Running',
    'explore.basket.count': { one: '{count} item', other: '{count} items' },
    'overview.hero.title': '{volume} sure to free',
    'only.en': 'English only',
  },
  'zh-TW': {
    'explore.basket.count': { other: '{count} 個項目' },
    'overview.hero.title': '{volume} 確定可釋放',
  },
}

describe('t', () => {
  const en = createTranslator('en', catalogs)
  const zh = createTranslator('zh-TW', catalogs)

  it('substitutes params', () => {
    expect(en.t('overview.hero.title', { volume: 'volume1' })).toBe('volume1 sure to free')
    expect(zh.t('overview.hero.title', { volume: 'volume1' })).toBe('volume1 確定可釋放')
  })

  // SDK messages: plural forms by values.count (Intl plural categories).
  it('selects the plural form by count with the SDK format', () => {
    expect(en.t('explore.basket.count', { count: 1 })).toBe('1 item')
    expect(en.t('explore.basket.count', { count: 3 })).toBe('3 items')
    expect(zh.t('explore.basket.count', { count: 1 })).toBe('1 個項目')
  })

  it('falls back to English, then to the key', () => {
    expect(zh.t('only.en')).toBe('English only')
    expect(zh.t('no.such.key')).toBe('no.such.key')
  })

  it('maps CLI codes and names unknown ones', () => {
    expect(en.code('state', 'running')).toBe('Running')
    expect(en.code('state', 'exploded')).toBe('Unrecognised status (exploded)')
  })
})

describe('locale detection', () => {
  it('maps every DSM language through the SDK', () => {
    expect(detectLocale({ dsmLang: 'cht', navigatorLanguages: ['en-US'] })).toBe('zh-TW')
    expect(detectLocale({ dsmLang: 'krn', navigatorLanguages: [] })).toBe('ko')
    expect(detectLocale({ dsmLang: 'rus', navigatorLanguages: ['fr-FR'] })).toBe('en')
  })

  it('uses the browser language outside DSM', () => {
    expect(detectLocale({ navigatorLanguages: ['fr-FR'] })).toBe('fr')
    expect(detectLocale({ navigatorLanguages: ['zh-TW'] })).toBe('zh-TW')
    expect(detectLocale({ navigatorLanguages: [] })).toBe('en')
  })
})
