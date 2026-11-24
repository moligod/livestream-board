import test from 'node:test'
import assert from 'node:assert/strict'
import { arrivingLedgerRows } from '../src/ledgerRows.ts'

const window = (latest_record_id: number, ids: number[], page = 1) => ({ page, latest_record_id, records: ids.map((row_id) => ({ row_id })) })
const previous = { scope: 'all', summary: window(30, [29, 28]) }

test('only genuinely inserted, visible rows on the same first page animate', () => {
  assert.deepEqual([...arrivingLedgerRows(previous, 'all', window(33, [33, 32, 30, 29]))], [33, 32])
  assert.deepEqual([...arrivingLedgerRows(previous, 'all', window(31, [30, 29, 28]))], [])
})
test('initial load, refresh, filters, pagination and old-server responses never animate existing rows', () => {
  const rows = window(33, [33, 29])
  assert.equal(arrivingLedgerRows(null, 'all', rows).size, 0)
  assert.equal(arrivingLedgerRows(previous, 'codex', rows).size, 0)
  assert.equal(arrivingLedgerRows(previous, null, rows).size, 0)
  assert.equal(arrivingLedgerRows(previous, 'all', window(33, [33], 2)).size, 0)
  assert.equal(arrivingLedgerRows(previous, 'all', { page: 1, records: [{}] }).size, 0)
  assert.equal(arrivingLedgerRows(previous, 'all', window(30, [29, 28])).size, 0)
})
