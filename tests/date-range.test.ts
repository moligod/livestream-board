import assert from 'node:assert/strict'
import test from 'node:test'
import { calendarMonthDays, localDateTimeParts, parseLocalDateTime } from '../src/dateRange.ts'

test('local date and time values round-trip without a UTC date shift', () => {
  const instant = new Date(2026, 10, 3, 5, 47, 0).getTime() / 1000
  const parts = localDateTimeParts(instant)
  assert.deepEqual(parts, { date: '2026-11-03', time: '05:47' })
  assert.equal(parseLocalDateTime(parts.date, parts.time), instant)
  assert.equal(parseLocalDateTime('2026-02-30', '12:00'), null)
  assert.equal(parseLocalDateTime('2026-11-03', ''), null)
  assert.equal(parseLocalDateTime('2026-11-03', '24:00'), null)
})

test('calendar grids include adjacent months and leap days in local time', () => {
  const days = calendarMonthDays(2024, 1)
  assert.equal(days.length, 42)
  assert.equal(days[0].date, '2024-01-28')
  assert.equal(days[0].inMonth, false)
  assert.equal(days.find((day) => day.date === '2024-02-29')?.inMonth, true)
  assert.equal(days[41].date, '2024-03-09')
})

test('nonexistent times during a daylight saving transition are rejected', () => {
  const previous = process.env.TZ
  process.env.TZ = 'America/Los_Angeles'
  try {
    assert.equal(parseLocalDateTime('2026-03-08', '02:30'), null)
    assert.notEqual(parseLocalDateTime('2026-03-08', '03:30'), null)
  } finally {
    if (previous === undefined) delete process.env.TZ
    else process.env.TZ = previous
  }
})
