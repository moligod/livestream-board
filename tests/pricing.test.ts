import assert from 'node:assert/strict'
import test from 'node:test'
import { estimateRecordCost, priceUsageRecords } from '../src/pricing.ts'
import { selectCatalogResults } from '../src/pricingSelection.ts'
import type { CatalogModel, ModelPrice, UsageRecord } from '../src/types.ts'

const price: ModelPrice = { model_id: 'model-a', name: 'Model A', provider: 'example', input: 2, output: 10, cache_read: 0.2, cache_write: 2.5, origin: 'manual' }
const record: UsageRecord = { source: 'codex', session_id: 's', timestamp: 1, project: 'p', model: 'model-a', input_tokens: 100, output_tokens: 20, reasoning_tokens: 5, cache_read_tokens: 300, cache_write_tokens: 40, total_tokens: 460, cost: 0 }

test('estimated pricing uses fresh input, output and cache components once', () => {
  const result = estimateRecordCost(record, price)
  assert.equal(result.cost_status, 'estimated')
  assert.equal(result.cost, (100 * 2 + 20 * 10 + 300 * 0.2 + 40 * 2.5) / 1_000_000)
  assert.equal(record.cost, 0)
})

test('Trae prompt totals include cached input and must not be charged twice', () => {
  const result = estimateRecordCost({ ...record, source: 'trae', input_tokens: 440 }, price)
  assert.equal(result.cost, (100 * 2 + 20 * 10 + 300 * 0.2 + 40 * 2.5) / 1_000_000)
})

test('reported costs take precedence and missing cache prices stay unpriced', () => {
  const reported = estimateRecordCost({ ...record, cost: 0.123 }, price)
  assert.equal(reported.cost, 0.123)
  assert.equal(reported.cost_status, 'reported')
  const missing = estimateRecordCost(record, { ...price, cache_write: null })
  assert.equal(missing.cost_status, 'unpriced')
  assert.equal(missing.cost, 0)
  const zeroCache = estimateRecordCost({ ...record, cache_write_tokens: 0 }, { ...price, cache_write: null })
  assert.equal(zeroCache.cost_status, 'estimated')
})

test('model lookup is case-insensitive and supports provider-qualified names', () => {
  const results = priceUsageRecords([{ ...record, model: 'example/MODEL-A' }, { ...record, model: 'unknown' }], [price])
  assert.equal(results[0].cost_status, 'estimated')
  assert.equal(results[1].cost, 0)
  assert.equal(results[1].cost_status, undefined)
})

function candidate(provider: string, model: string): CatalogModel {
  return { ...price, model_id: model, key: `${provider}/${model}`, name: model, provider, origin: 'models.dev' }
}

test('select all includes all matching models, not just the first visible 150', () => {
  const catalog = Array.from({ length: 300 }, (_, index) => candidate('vendor', `model-${index}`))
  const result = selectCatalogResults(new Set(), catalog, new Map(catalog.map((item) => [item.key, item])))
  assert.equal(result.size, 300)
  assert.ok(result.has('vendor/model-299'))
})

test('select all keeps existing providers and avoids duplicate model IDs', () => {
  const rows = [candidate('302ai', 'gpt-6.1-terra'), candidate('openai', 'gpt-6.1-terra'), candidate('anthropic', 'claude-4'), candidate('other', 'claude-4'), candidate('first', 'unknown-model'), candidate('second', 'unknown-model')]
  const byKey = new Map(rows.map((item) => [item.key, item]))
  const result = selectCatalogResults(new Set(['other/claude-4', 'saved/unavailable']), rows, byKey)
  assert.deepEqual([...result], ['other/claude-4', 'saved/unavailable', 'openai/gpt-6.1-terra', 'first/unknown-model'])
  assert.ok(!result.has('anthropic/claude-4'))
})

test('select all only adds filtered matches and retains unrelated selections', () => {
  const catalog = [candidate('openai', 'gpt-5'), candidate('google', 'gemini-2.5-pro'), candidate('other', 'gemini-2.5-pro')]
  const byKey = new Map(catalog.map((item) => [item.key, item]))
  const result = selectCatalogResults(new Set(['openai/gpt-5']), catalog.slice(1), byKey)
  assert.deepEqual([...result], ['openai/gpt-5', 'google/gemini-2.5-pro'])
})
