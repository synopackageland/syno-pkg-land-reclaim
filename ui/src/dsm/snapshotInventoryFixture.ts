// De-identified DSM inventory for the dev page and tests (scenario
// `snapshot_targets`): what the administrator's DSM session would answer
// for the LUN, the VMM disk and share01's snapshots in
// fixtures/snapshots.list/volume1-targets.json.
import { parseVmmSnapshots } from './snapshotInventory'
import type { DsmInventory, DsmInventorySource, DsmSnapshotState } from './snapshotInventory'

const DATA = {
  "luns": [
    {
      "uuid": "7d4e9b20-5a3c-4c6d-8e3f-1a2b3c4d5e6f",
      "name": "vdisk.1a2b3c4d-7e8f-4a0b-9c1d-2e3f4a5b6c7d.7d4e9b20-5a3c-4c6d-8e3f-1a2b3c4d5e6f.2.0",
      "type": "VDISK_BLUN"
    },
    {
      "uuid": "3f0c2a1e-6b7d-4a5b-9c2d-0a1b2c3d4e5f",
      "name": "esxi-datastore",
      "type": "BLUN"
    }
  ],
  "guests": [
    {
      "guest_id": "1a2b3c4d-7e8f-4a0b-9c1d-2e3f4a5b6c7d",
      "name": "ubuntu-dev"
    }
  ],
  "lunSnapshots": [
    {
      "uuid": "5c6d7e8f-0001-4a00-9b00-000000000001",
      "name": "SnapShot-1",
      "locked": true,
      "immutable": false
    },
    {
      "uuid": "5c6d7e8f-0002-4a00-9b00-000000000002",
      "name": "SnapShot-2",
      "locked": false,
      "immutable": true
    },
    {
      "uuid": "5c6d7e8f-0003-4a00-9b00-000000000003",
      "name": "SnapShot-3",
      "locked": false,
      "immutable": false
    },
    {
      "uuid": "5c6d7e8f-0004-4a00-9b00-000000000004",
      "name": "SnapShot-4",
      "locked": false,
      "immutable": false
    },
    {
      "uuid": "0b1c2d3e-0001-4f00-8a00-000000000001",
      "name": "before-upgrade",
      "locked": true,
      "immutable": false
    },
    {
      "uuid": "0b1c2d3e-0002-4f00-8a00-000000000002",
      "name": "after-upgrade",
      "locked": false,
      "immutable": false
    }
  ],
  "shares": {
    "share01": [
      {
        "time": "GMT+08_2026-09-27-00-00",
        "locked": false,
        "immutable": false
      },
      {
        "time": "GMT+08_2026-09-06-00-00",
        "locked": true,
        "immutable": false
      },
      {
        "time": "GMT+08_2026-08-02-00-00",
        "locked": false,
        "immutable": false
      },
      {
        "time": "GMT+08_2026-07-05-00-00",
        "locked": false,
        "immutable": true
      },
      {
        "time": "GMT+08_2026-06-01-00-00",
        "locked": false,
        "immutable": false
      },
      {
        "time": "GMT+08_2026-05-03-00-00",
        "locked": false,
        "immutable": false
      }
    ]
  }
} as const

export function fixtureInventory(): DsmInventory {
  return {
    luns: new Map(DATA.luns.map(lun => [lun.uuid, { name: lun.name, type: lun.type }])),
    vmmSnapshots: parseVmmSnapshots({ gsnaps: [
      { gsnap_time: Date.parse('2026-10-22T16:00:02Z') / 1000, desc: 'Daily snapshot', is_user_locked: false,
        is_main_exist: true, guest_disks: [{ blun_id: DATA.luns[0].uuid }] },
      { gsnap_time: Date.parse('2026-10-24T16:00:02Z') / 1000, desc: 'Protected daily snapshot', is_user_locked: true,
        is_main_exist: true, guest_disks: [{ blun_id: DATA.luns[0].uuid }] },
    ] }),
    guests: new Map(DATA.guests.map(guest => [guest.guest_id, guest.name])),
    lunSnapshots: new Map<string, DsmSnapshotState>(DATA.lunSnapshots.map(s => [s.uuid, { name: s.name, locked: s.locked, immutable: s.immutable }])),
  }
}

export const fixtureInventorySource: DsmInventorySource = {
  inventory: async () => fixtureInventory(),
  shareSnapshots: async share => {
    const rows = (DATA.shares as Record<string, readonly { time: string; locked: boolean; immutable: boolean }[]>)[share]
    return rows ? new Map(rows.map(row => [row.time, { locked: row.locked, immutable: row.immutable }])) : null
  },
}
