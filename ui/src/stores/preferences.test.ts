import { preferences, readPreferences, setSystemPartitionWarning, SYSTEM_PARTITION_WARNING_KEY } from './preferences'

describe('preferences store', () => {
  beforeEach(() => {
    localStorage.clear()
    readPreferences()
  })

  it('shows the system partition warning by default', () => {
    expect(SYSTEM_PARTITION_WARNING_KEY).toBe('synoreclaim.settings.systemPartitionWarning')
    expect(preferences.systemPartitionWarning).toBe(true)
  })

  it('keeps the warning off in this browser once turned off', () => {
    setSystemPartitionWarning(false)
    expect(preferences.systemPartitionWarning).toBe(false)
    expect(localStorage.getItem(SYSTEM_PARTITION_WARNING_KEY)).toBe('false')
    preferences.systemPartitionWarning = true
    readPreferences()
    expect(preferences.systemPartitionWarning).toBe(false)
  })

  it('shows the warning when the stored value is not a boolean', () => {
    localStorage.setItem(SYSTEM_PARTITION_WARNING_KEY, '"off"')
    readPreferences()
    expect(preferences.systemPartitionWarning).toBe(true)
  })

  it('still turns the warning off when storage is blocked', () => {
    vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => {
      throw new Error('blocked')
    })
    setSystemPartitionWarning(false)
    expect(preferences.systemPartitionWarning).toBe(false)
    vi.restoreAllMocks()
  })
})
