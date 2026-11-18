import { createRecycleBinSource, parseRecycleBins } from './recycleBins'

describe('recycle bins', () => {
  it('reads each share\'s Recycle Bin from SYNO.Core.Share list', () => {
    const bins = parseRecycleBins({ shares: [
      { name: 'main', enable_recycle_bin: true, recycle_bin_admin_only: false },
      { name: 'archive', enable_recycle_bin: false, recycle_bin_admin_only: false },
      { name: 'staff', enable_recycle_bin: true, recycle_bin_admin_only: true },
      // DSM without the field: unknown, so not listed.
      { name: 'usb' },
    ] })
    expect(bins).toEqual(new Map([
      ['main', { enabled: true, adminOnly: false }],
      ['archive', { enabled: false, adminOnly: false }],
      ['staff', { enabled: true, adminOnly: true }],
    ]))
    expect(parseRecycleBins({})).toBeNull()
  })

  it('asks DSM for the recyclebin fields and reads a failure as unknown', async () => {
    const webApi = vi.fn().mockResolvedValue({ shares: [{ name: 'main', enable_recycle_bin: true }] })
    expect(await createRecycleBinSource({ webApi })()).toEqual(new Map([['main', { enabled: true, adminOnly: false }]]))
    expect(webApi).toHaveBeenCalledWith('SYNO.Core.Share', 'list', 1, { additional: ['recyclebin'] })
    expect(await createRecycleBinSource({ webApi: vi.fn().mockRejectedValue({ code: 'dsm_105' }) })()).toBeNull()
    expect(await createRecycleBinSource({})()).toBeNull()
  })
})
