import { afterEach, describe, expect, it } from 'vitest'
import { synoTokenHeaders } from './synoToken'

afterEach(() => {
  delete window.SYNO
})

describe('synoTokenHeaders', () => {
  it('sends the DSM session token the desktop holds now', () => {
    window.SYNO = { SDS: { Session: { SynoToken: 'first' } } }
    expect(synoTokenHeaders()).toEqual({ 'X-SYNO-TOKEN': 'first' })
    window.SYNO.SDS!.Session!.SynoToken = 'renewed'
    expect(synoTokenHeaders()).toEqual({ 'X-SYNO-TOKEN': 'renewed' })
  })

  it('sends no header outside a signed-in DSM desktop', () => {
    expect(synoTokenHeaders()).toEqual({})
    window.SYNO = { SDS: { Session: { SynoToken: '' } } }
    expect(synoTokenHeaders()).toEqual({})
  })
})
