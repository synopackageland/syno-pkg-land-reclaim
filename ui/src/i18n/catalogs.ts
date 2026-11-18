// Message files: src/i18n/<name>/<lang>.json (the SDK's catalog layout),
// merged at build. Each file holds only keys starting with `<name>.`
// (codes: `code.`), so page threads never edit the same file.
import { mergeCatalogs, uiLocales } from '@synopackageland/sdk'
import type { Catalog, Locale } from './t'

const files = import.meta.glob<Catalog>('./*/*.json', { eager: true, import: 'default' })

export const catalogFiles: Record<string, Record<string, Catalog>> = {}
const folders: Record<string, Record<string, Catalog>> = {}
for (const [path, messages] of Object.entries(files)) {
  const [, name, lang] = path.match(/^\.\/([^/]+)\/([^/]+)\.json$/)!
  ;(catalogFiles[lang] ??= {})[name] = messages
  ;(folders[name] ??= {})[path] = messages
}

export function keyPrefix(fileName: string) {
  return fileName === 'codes' ? 'code' : fileName
}

/** Every page folder layered with the SDK's mergeCatalogs: a key a locale
 * lacks falls back to English. */
export const catalogs: Partial<Record<Locale, Catalog>> = Object.values(folders).reduce<Record<Locale, Catalog>>(
  (merged, folder) => mergeCatalogs(merged, folder),
  Object.fromEntries(uiLocales.map(locale => [locale, {}])) as Record<Locale, Catalog>,
)
