// Serves api() from recorded CLI JSON in ui/fixtures/ (layout and lookup
// rules: ui/fixtures/README.md). Dev server and tests only.
import { attributionFixture, type AttributionMode } from './attributionFixtures'
import { ApiError, apiErrorFromCgi, apiErrorFromDocument } from './errors'
import type { Backend } from './methods'
import { withPlanningFixtureJobs } from './planningFixtureJobs'
import { withDuplicatesFixture } from './duplicatesFixture'

export type FixtureEntry = {
  file?: string
  params?: Record<string, unknown>
  scenario?: string
  exit?: number
  /** Instead of `file`: the CLI document itself, inline. */
  document?: Record<string, unknown>
  /** Instead of `file`: the SDK envelope error. */
  error?: { code: string; message?: string }
}

const ATTRIBUTION_SCENARIOS: Partial<Record<string, AttributionMode>> = { unknown_small: 'small', unknown_large: 'large', coverage_gap: 'gap', namespace_churn: 'churn', namespace_churn_many: 'churn_many' }

export const SCENARIOS = [
  'normal',
  'ext4',
  'no_report',
  'first_scan',
  'scanning',
  'system_scanning',
  'job_recovery',
  'share_scanning',
  'paused',
  'paused_entries',
  'partial',
  'unknown_small',
  'unknown_large',
  'coverage_gap',
  'namespace_churn',
  'namespace_churn_many',
  'partial_stale',
  'stale',
  'empty',
  'changed',
  'age_warning',
  'detail_refusal',
  'busy',
  'invalid_param',
  'error',
  'not_admin',
  'days',
  'many_snapshots',
  'no_snapshots',
  'share_scan',
  'changed_system',
  'changed_share',
  'sysvol_getting_full',
  'sysvol_full',
  'sysvol_partial',
  'sysvol_small',
  'sysvol_nothing_movable',
  'sysvol_deep_short',
  'review_target_changed',
  'review_selected_changed',
  'review_snapshot_missing',
  'review_selection_partial',
  'review_partial_unrelated',
  'review_cleanup',
  'review_cleanup_refused',
  'review_long_refusal',
  'many_volumes',
  'unsupported_skipped',
  'no_supported_volume',
  'usage_change',
  'why_pending',
  'packages',
  'packages_own',
  'abb_share',
  'snapshot_unknown',
  'snapshot_targets',
  'small_items',
  'duplicates_none',
  'duplicates_stale',
  'duplicates_unavailable',
] as const

const indexes = import.meta.glob<FixtureEntry[]>('../../fixtures/*/index.json', { import: 'default' })
const documents = import.meta.glob<unknown>(['../../fixtures/*/*.json', '!../../fixtures/*/index.json'], {
  import: 'default',
})

function sameValue(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

function paramsMatch(entry: FixtureEntry, params: Record<string, unknown>) {
  return Object.entries(entry.params ?? {}).every(([key, value]) => sameValue(params[key], value))
}

export function findEntry(entries: FixtureEntry[], params: Record<string, unknown>, scenario: string) {
  const inScenario = (name: string) => entries.filter(e => (e.scenario ?? 'normal') === name)
  return [...inScenario(scenario), ...inScenario('normal')].find(e => paramsMatch(e, params))
}

export function fixtureBackend(options: { scenario?: string } = {}): Backend {
  return withPlanningFixtureJobs(withDuplicatesFixture(recordedFixtureBackend(options), { scenario: options.scenario }))
}

/** Recorded native worker documents, for composing development-only fixtures. */
export function recordedFixtureBackend({ scenario = 'normal' }: { scenario?: string } = {}): Backend {
  // The one setting the dev page can change; a real backend would persist it.
  let mutationsDisabled: boolean | undefined
  return async (method, params) => {
    if (method === 'setup.cleanup') {
      mutationsDisabled = (params as { mutations_disabled: boolean }).mutations_disabled
      return {
        schema_version: 1,
        command: 'config-set',
        tool_version: '0.1.0',
        generated_at: new Date().toISOString(),
        completion: 'complete',
        job_id: null,
        errors: [],
        warnings: [],
        enables_cleanup: !mutationsDisabled,
        config: { cleanup_enabled: true, mutations_disabled: mutationsDisabled },
      } as never
    }
    // The optional explanations never answer, so the Overview can be looked at
    // while they still load.
    if (scenario === 'why_pending' && method === 'why_not_freed.get') return new Promise<never>(() => {})
    const folder = `../../fixtures/${method}`
    const entries = (await indexes[`${folder}/index.json`]?.()) ?? []
    const entry = findEntry(entries, params as Record<string, unknown>, scenario)
    if (entry?.error) throw apiErrorFromCgi(entry.error.code)
    const load = entry?.document ? async () => entry.document : entry?.file ? documents[`${folder}/${entry.file}`] : undefined
    if (!load) throw new ApiError('fixture_missing', undefined, { method, params, scenario })
    const document = structuredClone(await load())
    // As the CGI backend does: a CLI refusal arrives as its document.
    const refusal = apiErrorFromDocument(document)
    if (refusal) throw refusal
    if (method === 'session.get' && mutationsDisabled !== undefined) Object.assign(document as object, { mutations_disabled: mutationsDisabled })
    const mode = ATTRIBUTION_SCENARIOS[scenario]
    return (mode ? attributionFixture(document, mode) : document) as never
  }
}

/** `?scenario=<name>` of the dev server URL. */
export function scenarioFromUrl(search = window.location.search) {
  return new URLSearchParams(search).get('scenario') ?? 'normal'
}
