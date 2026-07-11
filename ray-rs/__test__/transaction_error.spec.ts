import test from 'ava'

import { Kite } from '../ts/index'

function makeCommitFailureDb(error: Error): Kite {
  let active = false
  return {
    begin: () => {
      active = true
    },
    commit: () => {
      // Simulate native commit removing the transaction before reporting an
      // error across the binding boundary.
      active = false
      throw error
    },
    rollback: () => {
      if (!active) {
        throw new Error('No active transaction')
      }
      active = false
    },
    hasTransaction: () => active,
  } as unknown as Kite
}

test('transaction rethrows the original commit error after native transaction removal', (t) => {
  const originalError = new Error('forced commit failure')
  const db = makeCommitFailureDb(originalError)

  const thrown = t.throws(() => Kite.prototype.transaction.call(db, () => undefined))

  t.is(thrown, originalError)
  t.is(thrown?.message, 'forced commit failure')
  t.notRegex(thrown?.message ?? '', /No active transaction/)
  t.false(db.hasTransaction())
})

test('batch rethrows the original commit error without rollback masking', (t) => {
  const originalError = new Error('forced batch commit failure')
  const db = makeCommitFailureDb(originalError)

  const thrown = t.throws(() => Kite.prototype.batch.call(db, [() => undefined]))

  t.is(thrown, originalError)
  t.is(thrown?.message, 'forced batch commit failure')
  t.notRegex(thrown?.message ?? '', /No active transaction/)
  t.false(db.hasTransaction())
})
