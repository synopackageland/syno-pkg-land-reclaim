import { afterEach, describe, expect, it, vi } from 'vitest'
import { invalidateAnswers } from '@/stores/answers'
import { ANSWER_CACHE_LIMIT, cachedAnswer, clearAnswers } from './answerCache'

afterEach(() => clearAnswers())

/** A loader that records which answers it built. */
function recorder() {
  const loads: string[] = []
  const load = (name: string) => async () => {
    loads.push(name)
    return name
  }
  return { loads, load }
}

describe('cachedAnswer', () => {
  it('answers the same method and params again without loading', async () => {
    const { loads, load } = recorder()
    const params = { target: '/volume1', kind: 'all' }

    expect(await cachedAnswer('top.list', params, load('first'))).toBe('first')
    expect(await cachedAnswer('top.list', params, load('second'))).toBe('first')
    expect(loads).toEqual(['first'])
  })

  it('treats the same params in another order as the same answer', async () => {
    const { loads, load } = recorder()

    await cachedAnswer('top.list', { target: '/volume1', kind: 'all' }, load('first'))
    await cachedAnswer('top.list', { kind: 'all', target: '/volume1' }, load('second'))
    expect(loads).toEqual(['first'])
  })

  it('shares one in-flight request between callers', async () => {
    const { loads, load } = recorder()
    const pending = cachedAnswer('top.list', { target: '/volume1' }, load('first'))

    expect(await cachedAnswer('top.list', { target: '/volume1' }, load('second'))).toBe('first')
    expect(await pending).toBe('first')
    expect(loads).toEqual(['first'])
  })

  it('loads again for other params, another method or another volume', async () => {
    const { loads, load } = recorder()

    await cachedAnswer('top.list', { target: '/volume1' }, load('one'))
    await cachedAnswer('top.list', { target: '/volume2' }, load('two'))
    await cachedAnswer('recycle.list', { target: '/volume1' }, load('three'))
    expect(loads).toEqual(['one', 'two', 'three'])
  })

  it('drops every answer when the reports change', async () => {
    const { loads, load } = recorder()
    const params = { target: '/volume1' }

    await cachedAnswer('top.list', params, load('before'))
    invalidateAnswers()
    await cachedAnswer('top.list', params, load('after'))
    expect(loads).toEqual(['before', 'after'])
  })

  it('does not keep a failed answer, so a retry asks again', async () => {
    const load = vi.fn()
      .mockRejectedValueOnce(new Error('busy'))
      .mockResolvedValueOnce('answer')

    await expect(cachedAnswer('top.list', { target: '/volume1' }, load)).rejects.toThrow('busy')
    await expect(cachedAnswer('top.list', { target: '/volume1' }, load)).resolves.toBe('answer')
    expect(load).toHaveBeenCalledTimes(2)
  })

  it('keeps at most the newest answers', async () => {
    const { loads, load } = recorder()
    for (let n = 0; n <= ANSWER_CACHE_LIMIT; n += 1) {
      await cachedAnswer('top.list', { n }, load(String(n)))
    }

    loads.length = 0
    await cachedAnswer('top.list', { n: 0 }, load('reload-0'))
    await cachedAnswer('top.list', { n: ANSWER_CACHE_LIMIT }, load('reload-last'))
    expect(loads).toEqual(['reload-0'])
  })

  it('keeps recently read answers over older ones', async () => {
    const { loads, load } = recorder()
    for (let n = 0; n < ANSWER_CACHE_LIMIT; n += 1) {
      await cachedAnswer('top.list', { n }, load(String(n)))
    }
    await cachedAnswer('top.list', { n: 0 }, load('touch-0'))

    loads.length = 0
    await cachedAnswer('top.list', { n: ANSWER_CACHE_LIMIT }, load('new'))
    await cachedAnswer('top.list', { n: 0 }, load('reload-0'))
    await cachedAnswer('top.list', { n: 1 }, load('reload-1'))
    expect(loads).toEqual(['new', 'reload-1'])
  })

  it('forgets everything when answers are cleared', async () => {
    const { loads, load } = recorder()
    await cachedAnswer('top.list', { target: '/volume1' }, load('before'))
    clearAnswers()
    await cachedAnswer('top.list', { target: '/volume1' }, load('after'))
    expect(loads).toEqual(['before', 'after'])
  })
})
