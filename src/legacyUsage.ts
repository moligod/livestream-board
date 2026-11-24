// Compatibility for a browser reconnecting to an older local server during upgrade.
import { groupRecords } from './data'
import { priceUsageRecords } from './pricing'
import type { UsageQuery } from './api'
import type { DashboardData, PricingState, UsageSummary } from './types'

let cached: { revision: number; dashboard: DashboardData } | null = null

export async function loadLegacyUsageSummary(filters: UsageQuery, signal?: AbortSignal): Promise<UsageSummary> {
  if (!cached || cached.revision !== filters.revision) {
    const response = await fetch('/api/dashboard', { signal })
    if (!response.ok) throw new Error(`dashboard request failed (${response.status})`)
    cached = { revision: filters.revision, dashboard: await response.json() as DashboardData }
  }
  const pricesResponse = await fetch('/api/pricing', { signal })
  if (!pricesResponse.ok) throw new Error(`pricing request failed (${pricesResponse.status})`)
  const prices = (await pricesResponse.json() as PricingState).prices
  const query = filters.query.trim().toLowerCase()
  const visible = cached.dashboard.records.filter((record) => !filters.hidden.includes(record.source))
  const records = priceUsageRecords(visible.filter((record) => (!query || `${record.project} ${record.model} ${record.source}`.toLowerCase().includes(query))
    && (filters.source === 'all' || record.source === filters.source)
    && (filters.model === 'all' || record.model === filters.model)
    && (filters.project === 'all' || record.project === filters.project)
    && (filters.start === null || record.timestamp >= filters.start)
    && (filters.end === null || record.timestamp <= filters.end)), prices)
  const totals = records.reduce((acc, record) => {
    acc.total += record.total_tokens; acc.input += record.input_tokens; acc.output += record.output_tokens
    acc.reasoning += record.reasoning_tokens; acc.cache += record.cache_read_tokens; acc.cost += record.cost
    if (record.cost_status === 'estimated' || record.cost > 0) acc.priced += 1
    else acc.unpriced += 1
    return acc
  }, { total: 0, input: 0, output: 0, reasoning: 0, cache: 0, cost: 0, priced: 0, unpriced: 0 })
  const page = Math.min(filters.page, Math.max(1, Math.ceil(records.length / 20)))
  return { revision: filters.revision, record_count: records.length, page, totals,
    models: [...new Set(cached.dashboard.records.map((record) => record.model).filter(Boolean))].sort((a, b) => a.localeCompare(b)),
    projects: [...new Set(cached.dashboard.records.map((record) => record.project).filter(Boolean))].sort(),
    board_groups: groupRecords(records, 'project'), model_groups: groupRecords(records, 'model'),
    trend_records: records, calendar_records: records,
    board_totals: !filters.includeBoard ? [] : [3600, 86400, 604800, 2592000, 7776000, 31536000].map((seconds) => visible.reduce((sum, record) => sum + (record.timestamp >= filters.now - seconds ? record.total_tokens : 0), 0)),
    records: records.slice((page - 1) * 20, page * 20),
  }
}
