// @vitest-environment node
import { readdirSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { API_ERROR_CODES } from '@/api/errors'
import type { Message } from '@synopackageland/sdk'
import { catalogFiles, catalogs, keyPrefix } from './catalogs'
import { NOTICE_CODES } from '@/pages/Explore/entryNotes'

// Every locale the DSM UI language can select.
const LOCALES = ['en', 'zh-TW', 'zh-CN', 'ja', 'ko', 'de', 'fr', 'es', 'it'] as const

const dsmSource = readFileSync(fileURLToPath(new URL('../../../src/dsm.rs', import.meta.url)), 'utf8')
const schemaDir = fileURLToPath(new URL('../../../schemas/v1/', import.meta.url))
const schema = (name: string) => JSON.parse(readFileSync(`${schemaDir}${name}.schema.json`, 'utf8'))
const overviewPart = () => schema('overview').allOf[1].properties.results.items.properties.parts.items
const srcDir = fileURLToPath(new URL('../', import.meta.url))
// Every key the app's code names as a literal first argument of t(); t() shows a key it cannot find as is.
const literalKeys = () => {
  const sources = readdirSync(srcDir, { recursive: true, encoding: 'utf8' }).filter(file => /\.(vue|ts)$/.test(file) && !file.endsWith('.test.ts'))
  return sources.flatMap(file => [...readFileSync(`${srcDir}${file}`, 'utf8').matchAll(/(?<![\w.$])t\(\s*(['"])([^'"]+)\1/g)].map(match => ({ file, key: match[2] })))
}
const keysOf = (lang: string) => Object.values(catalogFiles[lang] ?? {}).flatMap(Object.keys).sort()
const textOf = (message: Message | undefined) => (typeof message === 'object' ? Object.values(message).join(' ') : (message ?? ''))
const placeholdersOf = (message: Message | undefined) => [...new Set([...textOf(message).matchAll(/\{(\w+)\}/g)].map(match => match[1]))].sort()

describe('message files', () => {
  it.each(Object.entries(catalogFiles).flatMap(([lang, files]) => Object.entries(files).map(([name, messages]) => [lang, name, messages] as const)))(
    '%s/%s.json only holds its own prefix',
    (_lang, name, messages) => {
      const prefix = `${keyPrefix(name)}.`
      expect(Object.keys(messages).filter(key => !key.startsWith(prefix))).toEqual([])
    },
  )

  // SDK messages: a plural message is its forms, chosen by {count}.
  it.each(LOCALES)('%s words every count as {count} in SDK plural forms', lang => {
    for (const messages of Object.values(catalogFiles[lang] ?? {})) {
      for (const [key, message] of Object.entries(messages)) {
        expect({ key, n: textOf(message).includes('{n}') }).toEqual({ key, n: false })
        if (typeof message === 'object') {
          expect({ key, forms: Object.keys(message).filter(form => !['zero', 'one', 'two', 'few', 'many', 'other'].includes(form)) }).toEqual({ key, forms: [] })
          expect({ key, other: typeof message.other }).toEqual({ key, other: 'string' })
        }
        expect({ key, flatForm: /\.(zero|one|two|few|many)$/.test(key) }).toEqual({ key, flatForm: false })
      }
    }
  })

  // Text components print a message as is: Markdown marks would show.
  it.each(LOCALES)('%s holds plain text, no Markdown backticks', lang => {
    for (const messages of Object.values(catalogFiles[lang] ?? {})) {
      for (const [key, message] of Object.entries(messages)) {
        expect({ key, backtick: textOf(message).includes('`') }).toEqual({ key, backtick: false })
      }
    }
  })

  it('merges every page folder into each locale through the SDK', () => {
    expect(catalogs.en?.['explore.basket.count']).toEqual(catalogFiles.en.explore['explore.basket.count'])
    expect(catalogs['zh-TW']?.['settings.about.title']).toBe(catalogFiles['zh-TW'].settings['settings.about.title'])
    expect(Object.keys(catalogs.fr ?? {}).sort()).toEqual(keysOf('en'))
  })

  it('zh-TW has exactly the English keys', () => {
    expect(keysOf('zh-TW')).toEqual(keysOf('en'))
  })

  it.each(LOCALES)('%s has exactly the English keys', lang => {
    expect(keysOf(lang)).toEqual(keysOf('en'))
  })

  // With every locale holding exactly the English keys (above), a key in English is in all of them.
  it('has an English message for every key the code names', () => {
    const keys = literalKeys()
    expect(keys.length).toBeGreaterThan(500)
    const english = new Set(keysOf('en'))
    expect(keys.filter(({ key }) => !english.has(key))).toEqual([])
  })

  it.each(LOCALES)('%s keeps every placeholder', lang => {
    const enFiles = catalogFiles.en
    for (const [name, messages] of Object.entries(catalogFiles[lang] ?? {})) {
      for (const [key, text] of Object.entries(messages)) {
        const source = enFiles[name]?.[key]
        expect({ key, placeholders: placeholdersOf(text) }).toEqual({ key, placeholders: placeholdersOf(source) })
      }
    }
  })

  it('says Syno Reclaim never empties a Recycle Bin, and where DSM does', () => {
    const en = catalogFiles.en.recycle
    const zh = catalogFiles['zh-TW'].recycle

    expect(en['recycle.hint.title']).toContain('never empties a Recycle Bin')
    expect(en['recycle.hint.dsm']).toContain('Control Panel > Shared Folder > Action > Empty Recycle Bin')
    expect(Object.values(en).join(' ')).not.toMatch(/\b(cart|basket)\b/i)
    expect(zh['recycle.hint.title']).toContain('從不清空資源回收筒')
    expect(Object.values(zh).join(' ')).not.toContain('購物籃')
    expect(Object.values(zh).join(' ')).not.toContain('購物車')
  })

  // Report chips show consistency; History shows the scan outcome. The two sets must not share a word.
  it.each(['en', 'zh-TW'] as const)('%s keeps consistency words apart from completion words', lang => {
    const codes = catalogFiles[lang].codes
    const words = (set: string) => Object.entries(codes).filter(([key]) => key.startsWith(`code.${set}.`)).map(([, text]) => textOf(text))
    const completion = words('completion')
    expect(words('consistency').filter(text => completion.some(done => text.includes(done) || done.includes(text)))).toEqual([])
  })

  it.each(['en', 'zh-TW'] as const)('%s words a finished scan in History as the completion code does', lang => {
    const { codes, history } = catalogFiles[lang]
    expect(history['history.outcome.completed']).toBe(codes['code.completion.complete'])
    expect(history['history.outcome.partial']).toBe(codes['code.completion.partial'])
  })

  it('uses cleanup list wording throughout Explore copy', () => {
    const en = catalogFiles.en.explore
    const zh = catalogFiles['zh-TW'].explore

    expect(textOf(en['explore.basket.count'])).toContain('cleanup list')
    expect(en['explore.basket.summaryLead']).toContain('Cleanup list')
    expect(Object.values(en).join(' ')).not.toMatch(/\b(cart|basket)\b/i)
    expect(Object.values(zh).join(' ')).not.toContain('購物籃')
    expect(Object.values(zh).join(' ')).not.toContain('購物車')
    expect(zh['explore.basket.summaryLead']).toContain('清理清單')
    expect(zh['explore.basket.summaryHeld']).toContain('快照')
  })

  // The cleanup bar sets each text after the amount apart from it: a flex
  // item drops a plain leading space, so a no-break space or a full-width
  // bracket does it.
  it.each(LOCALES)('%s sets the cleanup bar text after the amount apart from it', lang => {
    const { explore } = catalogFiles[lang]
    for (const key of ['explore.basket.summaryDetails', 'explore.basket.summaryAfter']) {
      const text = textOf(explore[key])
      if (text) expect(text, key).toMatch(/^(\u00a0\S|（)/)
    }
  })

  // The user chose 快照 for zh-TW; only the DSM product name keeps its Latin spelling.
  it('says 快照 in zh-TW, keeping only the product name Snapshot Replication', () => {
    const texts = Object.values(catalogFiles['zh-TW']).flatMap(messages => Object.entries(messages))
    expect(texts.filter(([, text]) => /snapshot/i.test(textOf(text).replaceAll('Snapshot Replication', ''))).map(([key]) => key)).toEqual([])
  })

  // Only a package the runtime cannot start is stopped or damaged; DSM not
  // verifying the sign-in and a cleanup without root have causes of their own.
  it.each(LOCALES)('%s names the cause of auth_unavailable and not_root, not a stopped package', lang => {
    const { codes, review, rootfsClean, state } = catalogFiles[lang]
    const stopped = [textOf(codes['code.api.backend_unavailable']), textOf(state['state.backendUnavailable.title'])]
    const messages = [codes['code.api.auth_unavailable'], review['review.apiError.auth_unavailable'], rootfsClean['rootfsClean.error.notRoot']].map(textOf)
    expect(messages.filter(text => !text || stopped.some(s => text.includes(s.replace(/[.。]$/, ''))))).toEqual([])
  })

  it('maps every API error the Recycle Bin page can receive in both languages', () => {
    const apiErrors = [
      'no_permission',
      'session_timeout',
      'reload',
      'backend_unavailable',
      'invalid_param',
      'busy',
      'cli_error',
      'timeout',
      'exec_failed',
      'fixture_missing',
      'unknown',
    ]
    for (const lang of ['en', 'zh-TW'] as const) {
      const keys = keysOf(lang)
      expect(apiErrors.filter(value => !keys.includes(`code.api.${value}`))).toEqual([])
    }
  })

  // The Recycle Bin warning callout renders warnings[] through code('warning', …).
  // Every code the answer path can put there (src/commands.rs) needs a message.
  it('maps every Recycle Bin warning code in both languages', () => {
    const warningCodes = ['unresolved_references', 'stale_report', 'cleanup_record_unreadable', 'recycle_age_unavailable']
    for (const lang of ['en', 'zh-TW'] as const) {
      const keys = keysOf(lang)
      expect(warningCodes.filter(value => !keys.includes(`code.warning.${value}`))).toEqual([])
    }
  })

  // CLI code lists read from schemas/v1: every value needs a message (SPEC §7.2).
  it.each([
    ['completion', schema('common').$defs.completion.enum],
    ['state', schema('jobs').allOf[1].properties.jobs.items.properties.state.enum],
    ['consistency', schema('common').$defs.consistency.enum],
    ['change', schema('common').$defs.change_reason.anyOf.map((k: { properties: { kind: { const: string } } }) => k.properties.kind.const)],
    ['part', overviewPart().properties.kind.enum],
    ['unknown_reason', overviewPart().properties.reasons.items.properties.code.enum],
    ['api', API_ERROR_CODES],
  ])('maps every CLI %s code', (set, values: string[]) => {
    const en = keysOf('en')
    expect(values.filter(value => !en.includes(`code.${set}.${value}`))).toEqual([])
  })

  // Explore names every reason `inspect --explain` can give a path (src/document.rs
  // `explain` and the blocker names of src/model/accounting.rs), never a generic line.
  describe('path notice codes', () => {
    const rust = (file: string) => readFileSync(fileURLToPath(new URL(`../../../src/${file}`, import.meta.url)), 'utf8')
    const explainBody = () => {
      const source = rust('document.rs')
      const start = source.indexOf('pub fn explain(')
      return source.slice(start, source.indexOf('\n}\n', start))
    }
    const explainCodes = () => [...explainBody().matchAll(/Notice::new\(\s*"([a-z_]+)"/g)].map(match => match[1])
    const blockerNames = () => {
      const names = /const NAMES: \[&'static str; \d+\] = \[([^\]]+)\]/.exec(rust('model/accounting.rs'))?.[1] ?? ''
      return [...names.matchAll(/"([a-z_]+)"/g)].map(match => match[1])
    }

    it('reads the codes from the Rust sources', () => {
      expect(explainCodes().length).toBeGreaterThan(8)
      expect(blockerNames()).toContain('snapshot')
      expect(explainBody()).toContain('blocker_message(name)')
    })

    it('knows every code the CLI explains a path with', () => {
      const known: readonly string[] = NOTICE_CODES
      expect([...explainCodes(), ...blockerNames()].filter(value => !known.includes(value))).toEqual([])
    })

    it.each(LOCALES)('%s words every path notice code', lang => {
      const keys = keysOf(lang)
      expect(NOTICE_CODES.filter(value => !keys.includes(`code.notice.${value}`))).toEqual([])
    })
  })

  // The Packages page names each service line and its DSM place from the service table (src/dsm.rs).
  describe('service table names', () => {
    const serviceCodes = [...dsmSource.matchAll(/(?:root|service_at)\(\s*"([a-z0-9_]+)"/g)].map(match => match[1])
    const placeIds = [...dsmSource.matchAll(/Place \{\s*id: "([a-z0-9_]+)"/g)].map(match => match[1])

    it('reads the service table', () => {
      expect(serviceCodes.length).toBeGreaterThan(25)
      expect(placeIds.length).toBeGreaterThan(15)
    })

    it.each(LOCALES)('%s names every service of the table', lang => {
      const keys = keysOf(lang)
      expect(serviceCodes.filter(value => !keys.includes(`code.service.${value}`))).toEqual([])
    })

    it.each(LOCALES)('%s names every DSM place of the table', lang => {
      const keys = keysOf(lang)
      expect([...placeIds, 'other'].filter(value => !keys.includes(`code.manage_in.${value}`))).toEqual([])
    })
  })
})
