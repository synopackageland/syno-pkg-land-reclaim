import { answerKey, answersVersion, invalidateAnswers } from './answers'

describe('answers', () => {
  it('counts every invalidation', () => {
    const before = answersVersion.value
    invalidateAnswers()
    invalidateAnswers()
    expect(answersVersion.value).toBe(before + 2)
  })

  it('keys an answer by report version and params, whatever the param order', () => {
    expect(answerKey('overview.get', { volume: '/volume1', why: true }, 1))
      .toBe(answerKey('overview.get', { why: true, volume: '/volume1' }, 1))
  })

  it('gives a new report version or new params a different key', () => {
    expect(answerKey('overview.get', { volume: '/volume1' }, 1))
      .not.toBe(answerKey('overview.get', { volume: '/volume1' }, 2))
    expect(answerKey('overview.get', { volume: '/volume1' }, 1))
      .not.toBe(answerKey('overview.get', { volume: '/volume2' }, 1))
  })
})
