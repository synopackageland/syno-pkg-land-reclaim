import { joinParts, locale, t } from '@/i18n'
import type { Volume } from '@/types'

/** Why this app cannot clean up the volume, as a plain sentence in the user's language. */
export const cleanupUnavailableReason = (volume: Volume) =>
  t(`state.${volume.capabilities.cleanup_unavailable_reason ?? 'ext4_read_only'}`)

/** That cleanup is unavailable, and the reason the CLI gave (its code). */
export const cleanupRefusalText = (reasonCode: string) =>
  joinParts([t('state.cleanupUnavailable'), t(`state.${reasonCode}`)], locale)

/** That cleanup is unavailable on the volume, and why. */
export const cleanupUnavailableText = (volume: Volume) =>
  cleanupRefusalText(volume.capabilities.cleanup_unavailable_reason ?? 'ext4_read_only')
