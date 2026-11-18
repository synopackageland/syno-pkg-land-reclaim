// One rule for every place that joins translated fragments onto a single line
// (the usage-change banner merges one summary per volume, chips join a label
// with a status, …).
//
// A fragment that already ends a sentence must not be followed by the middle
// dot: "……結果可能已變動。 · volume2：……" reads as double punctuation. East
// Asian locales write sentences without a space, so the fragments follow each
// other directly ("……變更。volume2：……"); other locales keep the sentence space
// ("… changed. volume2: …"), which is why the dot is never produced either way.
//
// Fragments without sentence punctuation are labels, and keep the separator
// used across the UI: " · ".
import type { Locale } from './t'

/** Sentence-final punctuation, half- or full-width, plus the ellipsis. */
const SENTENCE_END = /[.。！？!?…]$/

/** East Asian locales end a sentence with their own ideograph, not a space. */
const NO_SENTENCE_SPACE: ReadonlySet<Locale> = new Set(['zh-TW', 'zh-CN', 'ja'])

export const LABEL_SEPARATOR = ' · '

/** Empty or blank fragments are dropped, so a separator only ever stands
 * between two fragments that both have content ("Shared folder", not
 * "Shared folder ·" when a target has no id). */
export function joinParts(parts: readonly string[], locale: Locale): string {
  const sentenceBreak = NO_SENTENCE_SPACE.has(locale) ? '' : ' '
  return parts.filter(part => part.trim() !== '').reduce(
    (joined, part, index, kept) =>
      index === 0 ? part : joined + (SENTENCE_END.test(kept[index - 1]) ? sentenceBreak : LABEL_SEPARATOR) + part,
    '',
  )
}


/** Join sentences into one paragraph, keeping punctuation between sentences
 * and omitting only the paragraph's final punctuation (except an ellipsis). */
export function joinSentences(parts: readonly string[], locale: Locale): string {
  const sentences = parts.map(part => part.trim()).filter(Boolean)
  return sentences.map((sentence, index) => {
    if (index === sentences.length - 1) return sentence.replace(/[.。!?！？]+$/, '')
    const punctuated = SENTENCE_END.test(sentence) ? sentence : sentence + (NO_SENTENCE_SPACE.has(locale) ? '。' : '.')
    return punctuated + (NO_SENTENCE_SPACE.has(locale) ? '' : ' ')
  }).join('')
}
