// Who is using the app and whether the backend runs.
import { reactive } from 'vue'
import { api, ApiError } from '@/api'

export type SessionStatus = 'loading' | 'ready' | 'not_admin' | 'backend_unavailable' | 'error'

export const session = reactive({
  status: 'loading' as SessionStatus,
  user: null as string | null,
  toolVersion: null as string | null,
  cleanupEnabled: false,
  mutationsDisabled: false,
  error: null as ApiError | null,
})

function statusOf(error: unknown): SessionStatus {
  if (!(error instanceof ApiError)) return 'error'
  if (error.code === 'no_permission') return 'not_admin'
  if (error.code === 'backend_unavailable') return 'backend_unavailable'
  return 'error'
}

export async function loadSession() {
  session.status = 'loading'
  try {
    const doc = await api('session.get', {})
    Object.assign(session, {
      status: doc.is_admin === false ? 'not_admin' : 'ready',
      user: doc.user ?? doc.requested_by,
      toolVersion: doc.tool_version,
      cleanupEnabled: doc.cleanup_enabled,
      mutationsDisabled: doc.mutations_disabled,
      error: null,
    })
  } catch (error) {
    Object.assign(session, { status: statusOf(error), error: error instanceof ApiError ? error : null })
  }
}
