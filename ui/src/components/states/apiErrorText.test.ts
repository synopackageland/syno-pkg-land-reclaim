import { lifecycleCopy } from '@synopackageland/sdk'
import { apiErrorFromCgi } from '@/api/errors'
import { locale } from '@/i18n'
import { apiErrorText } from './apiErrorText'

// A refusal from a stopped or starting package reads the SDK's own
// words for an action, not "Unknown error (error code: APP_STOPPED)".
describe('apiErrorText', () => {
  it.each([
    ['APP_STOPPED', 'stopped'],
    ['APP_STARTING', 'starting'],
    ['EXECUTOR_STARTING', 'starting'],
  ] as const)('words %s with the SDK lifecycle copy', (code, lifecycle) => {
    expect(apiErrorText(apiErrorFromCgi(code))).toBe(lifecycleCopy(lifecycle, locale, { for: 'action' }))
  })

  it('keeps the app message and code for other refusals', () => {
    expect(apiErrorText(apiErrorFromCgi('ROOT_FAILED'))).not.toBe(lifecycleCopy('stopped', locale, { for: 'action' }))
    expect(apiErrorText(apiErrorFromCgi('credential_not_ready'))).not.toContain('stopped')
  })
})


it.each(['checkpoint_unusable', 'job_incompatible'])('shows the safe resume reason and rescan action for %s', code => {
  const message = 'Saved subvolume identity or generation changed; start a fresh scan'
  const text = apiErrorText(apiErrorFromCgi(code, message))
  expect(text).toContain(message)
  expect(text).toContain(code)
})
