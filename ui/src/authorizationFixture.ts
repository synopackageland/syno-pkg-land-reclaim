// Test helper for suites that mount App.vue to exercise the shell and pages:
// the startup gate must be ready for them to reach the app. Suites that drive
// an unavailable check use the retry helpers below. The gate itself
// (checking, authorization_required, cancel, retry, race) is covered by
// src/App.test.ts and src/stores/authorization.test.ts with its own clients.
import { bindAuthorizationGate, configureAuthorization } from './stores/authorization'
import { fixtureAuthorizationClient } from './api/authorizationFixtures'

export function allowAuthorizationForTests() {
  configureAuthorization({ client: fixtureAuthorizationClient({ state: 'ready', reason: null }), onSessionExpired: () => {} })
}

/**
 * As if the user dismissed the Gate's dialog before every root action. Call
 * after the app is mounted (AppContent binds the real Gate); returns the undo.
 */
export function declineRootWork() {
  return bindAuthorizationGate(() => Promise.resolve(false))
}

/**
 * The Gate checks an unavailable status again after 1 s and 3 s before it
 * shows the dialog. Call before mounting; then `elapseGateRetries()`.
 */
export function fakeGateRetryTimers() {
  vi.useFakeTimers({ toFake: ['setTimeout'] })
}

export async function elapseGateRetries() {
  await vi.advanceTimersByTimeAsync(4000)
}
