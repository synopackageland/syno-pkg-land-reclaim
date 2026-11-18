import { setBackend } from '@/api'
import { fixtureBackend } from '@/api/fixtures'
import { loadVolumes, selectedVolume, unsupportedVolumes, volumes } from './volumes'

describe('volumes store', () => {
  it('loads volumes and selects the first by default', async () => {
    setBackend(fixtureBackend())
    await loadVolumes()
    expect(volumes.error).toBeNull()
    expect(selectedVolume.value?.mount.path).toBe('/volume1')
    expect(unsupportedVolumes.value).toEqual([])
  })

  it('lists the volumes skipped for their file system next to the Btrfs ones', async () => {
    setBackend(fixtureBackend({ scenario: 'unsupported_skipped' }))
    await loadVolumes()
    expect(selectedVolume.value?.mount.path).toBe('/volume1')
    expect(unsupportedVolumes.value).toEqual(['/volume2'])
  })

  it('lists them when the NAS has no Btrfs volume at all', async () => {
    setBackend(fixtureBackend({ scenario: 'no_supported_volume' }))
    await loadVolumes()
    expect(volumes.document?.volumes).toEqual([])
    expect(unsupportedVolumes.value).toEqual(['/volume1', '/volume2'])
  })
})
