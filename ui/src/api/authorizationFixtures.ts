// Test/dev stand-ins for the CGI authorization client (lane D tests, the dev
// server). The DSM bundle builds the real client from the package CGI instead.
import { createAuthorizationClient, type AuthorizationClient, type AuthorizationStatus } from './authorization'

/** The `data` of a successful authorization.status answer. */
export function authorizationStatusDocument(status: AuthorizationStatus) {
  return { command: 'authorization' as const, schema_version: 1 as const, state: status.state, reason: status.reason }
}

export function fixtureAuthorizationClient(status: AuthorizationStatus): AuthorizationClient {
  return createAuthorizationClient(async () => authorizationStatusDocument(status))
}

/** One answer per call, then the last one again, for retry and reopen tests. */
export function sequenceAuthorizationClient(...statuses: AuthorizationStatus[]): AuthorizationClient {
  let call = 0
  return createAuthorizationClient(async () => authorizationStatusDocument(statuses[Math.min(call++, statuses.length - 1)]))
}
