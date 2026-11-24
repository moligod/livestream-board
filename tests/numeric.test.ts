import assert from 'node:assert/strict'
import test from 'node:test'
import { numericRange } from '../src/data.ts'
import { formatCostAxisTick, formatCostDetail, formatTokenAxisTick } from '../src/axisFormat.ts'

test('numericRange handles a dashboard-sized record bucket without spreading arguments', () => {
  const values = Array.from({ length: 342_414 }, (_, index) => index + 1)

  assert.deepEqual(numericRange(values), { min: 1, max: 342_414 })
})

test('numericRange returns null for an empty collection', () => {
  assert.equal(numericRange([]), null)
})

test('chart axis labels distinguish adjacent costs and billion-token ticks', () => {
  const cost = [0, 0.35, 0.7, 1.05, 1.4].map(formatCostAxisTick)
  const tokens = [0, 500_000_000, 1_000_000_000, 1_500_000_000, 2_000_000_000].map(formatTokenAxisTick)
  assert.equal(new Set(cost).size, cost.length)
  assert.equal(new Set(tokens).size, tokens.length)
  assert.match(formatCostAxisTick(0.0000123), /0\.0000123/)
  assert.equal(formatCostAxisTick(0.00000001), '$1e-8')
  assert.equal(formatCostDetail(0.0000123), '$0.0000123')
  assert.equal(formatTokenAxisTick(1_500_000_000), '1.5B')
})
