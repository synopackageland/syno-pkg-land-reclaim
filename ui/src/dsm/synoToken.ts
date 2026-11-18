/**
 * The DSM session token for one request. The SDK server reads it as
 * `X-SYNO-TOKEN` beside the session cookie; without it DSM refuses the
 * session, so a missing token surfaces as a sign-in prompt.
 */
export function synoTokenHeaders(): Record<string, string> {
  const token = window.SYNO?.SDS?.Session?.SynoToken
  return token ? { 'X-SYNO-TOKEN': token } : {}
}
