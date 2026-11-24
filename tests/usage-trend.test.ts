import assert from 'node:assert/strict'
import test from 'node:test'
import { buildUsageTrend, chooseTrendGranularity, resolveTimeWindow } from '../src/usageTrend.ts'
import type { UsageRecord } from '../src/types.ts'

function record(timestamp: number, total = 1): UsageRecord {
  return { source: 'codex', session_id: 'test', timestamp, project: 'demo', model: 'model', input_tokens: total, output_tokens: 0, reasoning_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, total_tokens: total, cost: 0 }
}

test('one-hour selection has 60 minute buckets and includes both range boundaries exactly once', () => {
  const end = 1_700_003_600
  const window = resolveTimeWindow('hour', null, null, false, end)
  const start = end - 3600
  const result = buildUsageTrend([record(start - 1, 50), record(start, 1), record(start + 60, 2), record(end - 1, 3), record(end, 4), record(end + 1, 50)], window)
  assert.equal(result.granularity, 'minute')
  assert.equal(result.points.length, 60)
  assert.equal(result.points[0].total, 1)
  assert.equal(result.points[1].total, 2)
  assert.equal(result.points[59].total, 7)
  assert.equal(result.points[0].intervalStart, start)
  assert.equal(result.points[59].intervalEnd, end)
  assert.equal(result.points.reduce((sum, point) => sum + point.total, 0), 10)
  assert.equal(result.points.filter((point) => point.total === 0).length, 57)
})

test('each token component and cost use the same selected time window', () => {
  const start = 1_700_000_000
  const item = { ...record(start + 20, 12), input_tokens: 2, output_tokens: 3, cache_read_tokens: 7, cost: 0.125 }
  const outside = { ...item, timestamp: start - 1, total_tokens: 999 }
  const result = buildUsageTrend([item, outside], { start, end: start + 3600 })
  assert.deepEqual(result.points.reduce((sum, point) => ({ total: sum.total + point.total, input: sum.input + point.input, output: sum.output + point.output, cache: sum.cache + point.cache, cost: sum.cost + point.cost }), { total: 0, input: 0, output: 0, cache: 0, cost: 0 }), { total: 12, input: 2, output: 3, cache: 7, cost: 0.125 })
})

test('preset windows and granularity follow their full selected period', () => {
  const now = 1_700_000_000
  const day = buildUsageTrend([], resolveTimeWindow('day', null, null, false, now))
  assert.equal(day.granularity, 'hour')
  assert.equal(day.points.length, 24)
  assert.equal(day.points.every((point) => point.total === 0), true)
  for (const range of ['week', 'month'] as const) {
    const window = resolveTimeWindow(range, null, null, false, now)
    const result = buildUsageTrend([record(window.start!, 10), record(now, 20)], window)
    assert.equal(result.granularity, 'day')
    assert.equal(result.points.reduce((sum, point) => sum + point.total, 0), 30)
    assert.ok(result.points.length >= (range === 'week' ? 7 : 30))
  }
})

test('all and custom ranges include the full period rather than the newest 24 hours', () => {
  const start = 1_700_000_000
  const records = [record(start, 2), record(start + 10 * 86400, 3), record(start + 40 * 86400, 5)]
  const all = buildUsageTrend(records, resolveTimeWindow('all', null, null, false, start))
  assert.equal(all.granularity, 'day')
  assert.equal(all.points.reduce((sum, point) => sum + point.total, 0), 10)
  assert.ok(all.points.length >= 41)
  const custom = resolveTimeWindow('custom', start, start + 10 * 86400, false, start + 100 * 86400)
  assert.equal(buildUsageTrend(records, custom).points.reduce((sum, point) => sum + point.total, 0), 5)
  assert.equal(buildUsageTrend(records, { start: start + 1, end: start }).invalidRange, true)
  assert.equal(resolveTimeWindow('custom', start, start + 10, true, start + 20).end, start + 20)
})

test('daily buckets follow local calendar days through daylight saving changes', () => {
  const previous = process.env.TZ
  process.env.TZ = 'America/Los_Angeles'
  try {
    const start = Math.floor(new Date(2026, 2, 7, 12).getTime() / 1000)
    const end = Math.floor(new Date(2026, 2, 10, 12).getTime() / 1000)
    const result = buildUsageTrend([record(start), record(end)], { start, end })
    assert.equal(result.granularity, 'day')
    assert.equal(result.points.reduce((sum, point) => sum + point.total, 0), 2)
    assert.ok(result.points.some((point, index) => index > 0 && point.key - result.points[index - 1].key === 23 * 3600))
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})

test('large all-time sets use adaptive buckets without losing records', () => {
  const start = 1_600_000_000
  const records = Array.from({ length: 350_000 }, (_, index) => record(start + (index % 1200) * 86400))
  const result = buildUsageTrend(records, { start: null, end: null })
  assert.equal(result.granularity, 'week')
  assert.equal(result.points.reduce((sum, point) => sum + point.total, 0), records.length)
  assert.ok(result.points.length < 200)
  assert.equal(chooseTrendGranularity(40 * 366 * 86400), 'year')
})
