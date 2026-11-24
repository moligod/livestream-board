import type { ModelPrice, UsageRecord } from './types'

/** Prices are per million tokens; input is cache-exclusive except in Trae. */
export function estimateRecordCost(record: UsageRecord, price: ModelPrice | undefined): UsageRecord {
  if (record.cost > 0) return { ...record, cost_status: 'reported' }
  if (!price) return { ...record, cost_status: 'unpriced' }
  const cachedRead = Math.max(0, record.cache_read_tokens)
  const cachedWrite = Math.max(0, record.cache_write_tokens)
  const freshInput = record.source === 'trae' || record.source === 'trae-cn'
    ? Math.max(0, record.input_tokens - cachedRead - cachedWrite)
    : Math.max(0, record.input_tokens)
  if ((cachedRead > 0 && price.cache_read === null) || (cachedWrite > 0 && price.cache_write === null)) {
    return { ...record, cost_status: 'unpriced' }
  }
  const cost = (freshInput * price.input + Math.max(0, record.output_tokens) * price.output
    + cachedRead * (price.cache_read ?? 0) + cachedWrite * (price.cache_write ?? 0)) / 1_000_000
  return { ...record, cost, cost_status: 'estimated' }
}

export function priceUsageRecords(records: UsageRecord[], prices: ModelPrice[]): UsageRecord[] {
  if (!prices.length) return records
  const byModel = new Map(prices.map((price) => [price.model_id.toLowerCase(), price]))
  const lookup = new Map<string, ModelPrice | undefined>()
  return records.map((record) => {
    const name = record.model.toLowerCase()
    if (!lookup.has(name)) lookup.set(name, byModel.get(name) ?? byModel.get(name.split('/').at(-1) ?? name))
    const price = lookup.get(name)
    return record.cost > 0 || !price ? record : estimateRecordCost(record, price)
  })
}
