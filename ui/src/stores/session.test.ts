import { fixtureBackend } from '@/api/fixtures'
import { setBackend } from '@/api'
import { apiErrorFromCgi } from '@/api/errors'
import { loadSession, session } from './session'

describe('session store', () => {
  it('is ready for an administrator', async () => {
    setBackend(fixtureBackend())
    await loadSession()
    expect(session.status).toBe('ready')
    expect(session.cleanupEnabled).toBe(true)
  })

  it('is not_admin on error 105', async () => {
    setBackend(fixtureBackend({ scenario: 'not_admin' }))
    await loadSession()
    expect(session.status).toBe('not_admin')
  })

  // A failed root operation says nothing about whether the package runs.
  it('is an error, not backend_unavailable, when the root call fails', async () => {
    setBackend(fixtureBackend({ scenario: 'error' }))
    await loadSession()
    expect(session.status).toBe('error')
    expect(session.error?.detail?.cgi_code).toBe('ROOT_FAILED')
  })

  it('is backend_unavailable when the App cannot start', async () => {
    setBackend(() => Promise.reject(apiErrorFromCgi('app_unavailable')))
    await loadSession()
    expect(session.status).toBe('backend_unavailable')
  })
})
