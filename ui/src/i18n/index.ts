// The app's language and formatters, fixed for the window's lifetime (DSM
// reloads windows when its language changes).
import { catalogs } from './catalogs'
import { createFormatters } from './format'
import { createTranslator, detectLocale } from './t'

export type { Locale, Params } from './t'
export { formatVolume } from './format'
export { joinParts, joinSentences, LABEL_SEPARATOR } from './joinParts'

function dsmLang(): string | undefined {
  try {
    return window._S?.('lang') || window.SYNO?.SDS?.Session?.lang || undefined
  } catch {
    return undefined
  }
}

const translator = createTranslator(detectLocale({ dsmLang: dsmLang(), navigatorLanguages: navigator.languages }), catalogs)

export const { locale, t, code } = translator
export const { formatBytes, formatCount, formatPercent, formatDuration, formatDateTime, formatCompactDateTime, formatRelative } =
  createFormatters(translator)
