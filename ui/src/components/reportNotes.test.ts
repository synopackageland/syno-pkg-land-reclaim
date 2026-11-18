import { reportNotes } from './reportNotes'

const attribution = (extra = {}) => ({ unknown_bytes: 0, used_bytes: 100 * 2 ** 30, coverage_gaps: false, ...extra })

describe('reportNotes', () => {
  it('is empty for a complete, stable report', () => {
    expect(reportNotes({ consistency: 'observed-stable', attribution: attribution() }, [])).toEqual([])
  })

  it('says how much could not be attributed above the small threshold', () => {
    const [note] = reportNotes({ consistency: 'observed-stable', attribution: attribution({ unknown_bytes: 20 * 2 ** 30 }) }, [])
    expect(note).toContain('could not be attributed')
  })

  it('counts entries skipped because they changed during the scan', () => {
    const notes = reportNotes({ consistency: 'changed-during-scan', attribution: attribution({ namespace_churn: { dangling_entries: 1 } }) }, [])
    expect(notes).toContain('1 item changed during the scan and was skipped')
  })

  it('says when only DSM system areas changed', () => {
    const report = { consistency: 'changed-during-scan', change_reasons: [{ kind: 'subvolume-generation-changed', subvolume_id: 5 }], attribution: attribution() }
    expect(reportNotes(report, [{ subvolume_id: 257 }]).join(' ')).toContain('system')
  })

  it('leaves a coverage gap to the partial banner', () => {
    expect(reportNotes({ consistency: 'partial', attribution: attribution({ coverage_gaps: true, unknown_bytes: 20 * 2 ** 30 }) }, [])).toEqual([])
  })
})
