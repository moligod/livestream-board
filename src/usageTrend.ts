import type { UsageRecord } from './types'

export type TimeRange = 'hour' | 'day' | 'week' | 'month' | 'all' | 'custom'
export type TimeWindow = { start: number | null; end: number | null }
export type TrendGranularity = 'minute' | 'hour' | 'day' | 'week' | 'month' | 'year'
export type TrendPoint = { key: number; label: string; intervalStart: number; intervalEnd: number; total: number; input: number; output: number; cache: number; cost: number }
export type TrendResult = { points: TrendPoint[]; granularity: TrendGranularity; invalidRange: boolean }

const presetSeconds: Partial<Record<TimeRange, number>> = { hour: 3600, day: 86400, week: 7 * 86400, month: 30 * 86400 }

/** Shared by the summary, detail filter and chart. Both boundaries are inclusive. */
export function resolveTimeWindow(range: TimeRange, customStart: number | null, customEnd: number | null, liveEnd: boolean, now: number): TimeWindow {
  if (range === 'all') return { start: null, end: null }
  if (range === 'custom') return { start: customStart, end: liveEnd ? now : customEnd ?? now }
  return { start: now - (presetSeconds[range] ?? 86400), end: now }
}

export function chooseTrendGranularity(spanSeconds: number): TrendGranularity {
  if (spanSeconds <= 3600) return 'minute'
  if (spanSeconds <= 86400) return 'hour'
  if (spanSeconds <= 366 * 86400) return 'day'
  if (spanSeconds <= 7 * 366 * 86400) return 'week'
  if (spanSeconds <= 30 * 366 * 86400) return 'month'
  return 'year'
}

function localBoundary(timestamp: number, granularity: TrendGranularity): Date {
  const date = new Date(timestamp * 1000)
  date.setHours(0, 0, 0, 0)
  if (granularity === 'week') date.setDate(date.getDate() - ((date.getDay() + 6) % 7))
  if (granularity === 'month') date.setDate(1)
  if (granularity === 'year') date.setMonth(0, 1)
  return date
}

function advance(date: Date, granularity: TrendGranularity): void {
  if (granularity === 'day') date.setDate(date.getDate() + 1)
  else if (granularity === 'week') date.setDate(date.getDate() + 7)
  else if (granularity === 'month') date.setMonth(date.getMonth() + 1)
  else date.setFullYear(date.getFullYear() + 1)
}

function pointLabel(timestamp: number, granularity: TrendGranularity): string {
  const date = new Date(timestamp * 1000)
  const pad = (value: number) => String(value).padStart(2, '0')
  if (granularity === 'minute' || granularity === 'hour') return `${pad(date.getMonth() + 1)}/${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`
  if (granularity === 'year') return String(date.getFullYear())
  if (granularity === 'month') return `${date.getFullYear()}/${pad(date.getMonth() + 1)}`
  return `${date.getFullYear()}/${pad(date.getMonth() + 1)}/${pad(date.getDate())}`
}

/** Produce one zero-filled, range-limited series for the already filtered records. */
export function buildUsageTrend(records: readonly UsageRecord[], window: TimeWindow): TrendResult {
  let earliest = Number.POSITIVE_INFINITY
  let latest = Number.NEGATIVE_INFINITY
  for (const record of records) {
    if (!Number.isFinite(record.timestamp)) continue
    if (record.timestamp < earliest) earliest = record.timestamp
    if (record.timestamp > latest) latest = record.timestamp
  }
  const start = window.start ?? earliest
  const end = window.end ?? latest
  if (Number.isFinite(start) && Number.isFinite(end) && start > end) return { points: [], granularity: 'hour', invalidRange: true }
  if (!Number.isFinite(start) || !Number.isFinite(end)) return { points: [], granularity: 'hour', invalidRange: false }
  if (!Number.isFinite(new Date(start * 1000).getTime()) || !Number.isFinite(new Date(end * 1000).getTime())) return { points: [], granularity: 'hour', invalidRange: true }

  const granularity = chooseTrendGranularity(end - start)
  const boundaries: number[] = []
  if (granularity === 'minute' || granularity === 'hour') {
    const seconds = granularity === 'minute' ? 60 : 3600
    const count = Math.max(1, Math.ceil((end - start) / seconds))
    for (let index = 0; index < count; index += 1) boundaries.push(start + index * seconds)
  } else {
    const cursor = localBoundary(start, granularity)
    while (cursor.getTime() / 1000 <= end) {
      boundaries.push(cursor.getTime() / 1000)
      advance(cursor, granularity)
    }
  }
  const points = boundaries.map((key, index) => ({ key, label: pointLabel(key, granularity), intervalStart: Math.max(key, start), intervalEnd: Math.min(boundaries[index + 1] ?? end, end), total: 0, input: 0, output: 0, cache: 0, cost: 0 }))
  for (const record of records) {
    if (!Number.isFinite(record.timestamp) || record.timestamp < start || record.timestamp > end) continue
    // Find the last boundary at or before the event; this also handles DST days.
    let low = 0
    let high = boundaries.length
    while (low < high) {
      const middle = Math.floor((low + high) / 2)
      if (boundaries[middle] <= record.timestamp) low = middle + 1
      else high = middle
    }
    const point = points[Math.max(0, low - 1)]
    point.total += record.total_tokens
    point.input += record.input_tokens
    point.output += record.output_tokens
    point.cache += record.cache_read_tokens
    point.cost += record.cost
  }
  return { points, granularity, invalidRange: false }
}

export const granularityLabels: Record<TrendGranularity, string> = {
  minute: '每分钟', hour: '每小时', day: '每天', week: '每周', month: '每月', year: '每年',
}
