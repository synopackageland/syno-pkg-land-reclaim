import { lifecycleRefusal } from '@synopackageland/sdk'
import { ApiError, executorCode, sdkRefusal } from '@/api/errors'
import { code, locale, t } from '@/i18n'

/**
 * The message for a refused call, with the executor's code when it names the
 * failure; a stopped or starting package's refusal reads the SDK's words.
 */
export function apiErrorText(error: unknown): string {
  const lifecycle = lifecycleRefusal(sdkRefusal(error), locale)
  if (lifecycle) return lifecycle
  const apiError = error instanceof ApiError ? error : null
  const resumeMessage = apiError?.detail?.resume_message
  const message = typeof resumeMessage === 'string' ? resumeMessage : code('api', apiError?.code ?? 'unknown')
  const executor = apiError && executorCode(apiError)
  return executor ? t('common.errorCode', { message, code: executor }) : message
}
