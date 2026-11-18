// t() over the SDK's messages: flat catalogs per
// language, a plural message is its forms (`{one, other}`) chosen by
// `count` through the SDK's format, English fallback.
// Locales are the SDK's UI locales (§7.3).
import { format, uiLocale, type Message, type UiLocale } from '@synopackageland/sdk'

export type Locale = UiLocale
export type Catalog = Record<string, Message>
export type Params = Record<string, string | number>

export function createTranslator(locale: Locale, catalogs: Partial<Record<Locale, Catalog>>) {
  const own = catalogs[locale] ?? {}
  const fallback = catalogs.en ?? {}
  const lookup = (key: string) => own[key] ?? fallback[key]

  function t(key: string, params?: Params): string {
    return format(lookup(key) ?? key, params ?? {}, locale)
  }

  /** A CLI code (`code.<set>.<value>`), or "Unrecognised status (value)". */
  function code(set: string, value: string): string {
    const key = `code.${set}.${value}`
    return lookup(key) !== undefined ? t(key) : t('code.unknown', { code: value })
  }

  return { locale, t, code }
}

export type Translator = ReturnType<typeof createTranslator>

// DSM's language, else the browser's; the SDK maps both to a shipped locale.
export function detectLocale(env: { dsmLang?: string; navigatorLanguages: readonly string[] }): Locale {
  return uiLocale(env.dsmLang ?? env.navigatorLanguages[0] ?? '')
}
