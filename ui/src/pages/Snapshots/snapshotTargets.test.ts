import { describeTarget, snapshotManager } from './snapshotTargets'
import { fixtureInventory } from '@/dsm/snapshotInventoryFixture'
import targets from '../../../fixtures/snapshots.list/volume1-targets.json'

const labels = { unnamedLun: (id: string) => id, vmmImage: 'Image', vmmDisk: 'Disk', tiering: 'Tiering', unnamed: (id: string) => id }

it.each([
  ['share01', 'SYNO.SDS.DisasterRecovery.Application', 'snapshot_replication'],
  ['3f0c2a1e-6b7d-4a5b-9c2d-0a1b2c3d4e5f', 'SYNO.SDS.iSCSI.Application', 'san_manager'],
  ['7d4e9b20-5a3c-4c6d-8e3f-1a2b3c4d5e6f', 'SYNO.SDS.Virtualization.Application', 'virtual_machine_manager'],
])('routes %s by inventory type despite misleading display labels', (path, appId, placeId) => {
  const share = targets.results[0]!.shares.find(s => s.share.path === path)!
  const described = describeTarget({ ...share, manage_in: 'Misleading label' } as never, fixtureInventory(), labels)
  expect(snapshotManager(described.kind)).toEqual({ appId, placeId })
})
