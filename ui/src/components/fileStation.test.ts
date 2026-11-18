import { fileStationBlock, openInFileStation, type ShareVolume } from './fileStation'

const FILE_STATION = 'SYNO.SDS.App.FileStation3.Instance'

/** A DSM desktop stub that records File Station launches. */
function stubFileStation(enabled = true) {
  const calls: { id: string; params?: unknown }[] = []
  ;(window as unknown as { SYNO?: unknown }).SYNO = {
    SDS: {
      AppLaunch: (id: string, params?: unknown) => calls.push({ id, params }),
      StatusNotifier: { isAppEnabled: () => enabled },
    },
  }
  return calls
}

/** A volumes.list volume with the shared folders DSM lists on it. */
function volume(mount: string, shares: string[], notes: string[] = []): ShareVolume {
  return {
    mount: { path: mount },
    shares: shares.map(name => ({ name: { path: name }, path: { path: `${mount}/${name}` }, subvolume_id: null })),
    notes: notes.map(code => ({ code, message: code })),
  }
}

const VOLUMES = [
  volume('/volume1', ['share04', 'share03', 'ActiveBackupforBusiness']),
  volume('/volume2', ['media']),
  volume('/volume20', ['other']),
  volume('/volumeUSB1', ['usbshare1']),
]

afterEach(() => {
  delete (window as unknown as { SYNO?: unknown }).SYNO
})

describe('File Station targets', () => {
  it('opens a file in a share selected in its folder', () => {
    const calls = stubFileStation()
    expect(openInFileStation({ path: '/volume1/share04/folder0002/file 1.bin', kind: 'file' }, VOLUMES)).toBe(true)
    expect(calls).toEqual([{ id: FILE_STATION, params: { openfile: '/share04/folder0002/file 1.bin' } }])
  })

  it('opens a folder, a share and a nested share folder as themselves', () => {
    const calls = stubFileStation()
    openInFileStation({ path: '/volume1/share04/folder0002', kind: 'dir' }, VOLUMES)
    openInFileStation({ path: '/volume1/share03', kind: 'service' }, VOLUMES)
    openInFileStation({ path: '/volume1/share04/a/b/c/d', kind: 'dir' }, VOLUMES)
    expect(calls).toEqual([
      { id: FILE_STATION, params: { opendir: '/share04/folder0002' } },
      { id: FILE_STATION, params: { opendir: '/share03' } },
      { id: FILE_STATION, params: { opendir: '/share04/a/b/c/d' } },
    ])
  })

  it('maps a share on /volume2 and on /volumeUSB1 by its own volume', () => {
    const calls = stubFileStation()
    openInFileStation({ path: '/volume2/media/film.mkv', kind: 'file' }, VOLUMES)
    openInFileStation({ path: '/volumeUSB1/usbshare1/photos', kind: 'dir' }, VOLUMES)
    expect(calls).toEqual([
      { id: FILE_STATION, params: { openfile: '/media/film.mkv' } },
      { id: FILE_STATION, params: { opendir: '/usbshare1/photos' } },
    ])
  })

  it('does not take /volume2 for /volume20 or a share of another volume', () => {
    expect(fileStationBlock({ path: '/volume20/media/film.mkv', kind: 'file' }, VOLUMES)).toBe('not_shared')
    expect(fileStationBlock({ path: '/volume2/share04/x', kind: 'file' }, VOLUMES)).toBe('not_shared')
  })

  it.each([
    ['a top-level folder that is not a share', '/volume1/scratch/rootfs-1/volume-state.tar'],
    ['the top-level folder itself', '/volume1/scratch'],
    ['a share name that differs only in case', '/volume1/Share04/folder0002/file.bin'],
    ['a name that only starts like a share', '/volume1/share04x/file.bin'],
    ['DSM\'s @docker', '/volume1/@docker'],
    ['a package folder', '/volume1/@appstore/SynologyDrive'],
    ['Reclaim\'s own folder', '/volume1/@synoreclaim/rootfs-1'],
    ['the volume', '/volume1'],
    ['the volume with a slash', '/volume1/'],
    ['a relative path', 'relative/path'],
  ])('never offers %s and never launches it', (_name, path) => {
    const calls = stubFileStation()
    expect(fileStationBlock({ path, kind: 'file' }, VOLUMES)).not.toBeNull()
    expect(openInFileStation({ path, kind: 'file' }, VOLUMES)).toBe(false)
    expect(calls).toEqual([])
  })

  it('says not in a shared folder for a path on a known volume outside every share', () => {
    expect(fileStationBlock({ path: '/volume1/scratch/rootfs-1/volume-state.tar', kind: 'file' }, VOLUMES)).toBe('not_shared')
    expect(fileStationBlock({ path: '/volume1/Share04/file.bin', kind: 'file' }, VOLUMES)).toBe('not_shared')
  })

  it('says the share list is unknown when the volume is not listed or its shares could not be read', () => {
    expect(fileStationBlock({ path: '/volume3/share/x', kind: 'file' }, VOLUMES)).toBe('shares_unknown')
    expect(fileStationBlock({ path: '/volume1/share/x', kind: 'file' }, [volume('/volume1', [], ['shares_unavailable'])])).toBe('shares_unknown')
    expect(fileStationBlock({ path: '/volume1/share/x', kind: 'file' }, [])).toBe('shares_unknown')
  })

  it('keeps a share whose subfolder starts with @ open', () => {
    expect(fileStationBlock({ path: '/volume1/share04/@eaDir', kind: 'dir' }, VOLUMES)).toBeNull()
  })

  it('does not offer a name that is not UTF-8', () => {
    expect(fileStationBlock({ path: '/volume1/share04/caf�', path_hex: '2f766f6c', kind: 'file' }, VOLUMES)).toBe('unreadable_name')
  })

  it('answers false outside DSM and when File Station is disabled', () => {
    expect(openInFileStation({ path: '/volume1/share04', kind: 'dir' }, VOLUMES)).toBe(false)
    const calls = stubFileStation(false)
    expect(openInFileStation({ path: '/volume1/share04', kind: 'dir' }, VOLUMES)).toBe(false)
    expect(calls).toEqual([])
  })
})
