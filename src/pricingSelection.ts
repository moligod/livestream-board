import type { CatalogModel } from './types'

/** Prefer the model's own provider when a known family has several prices. */
function preferredProvider(modelId: string): string | undefined {
  const name = (modelId.toLowerCase().split('/').at(-1) ?? modelId).toLowerCase()
  if (/^(gpt|chatgpt|codex)(-|$)|^o[1-9](?:-|$)/.test(name)) return 'openai'
  if (/^claude(?:-|$)/.test(name)) return 'anthropic'
  if (/^(gemini|gemma)(?:\d|-|\.|$)/.test(name)) return 'google'
  if (/^grok(?:-|$)/.test(name)) return 'xai'
  if (/^deepseek(?:-|$)/.test(name)) return 'deepseek'
  if (/^qwen(?:\d|-|\.|$)/.test(name)) return 'alibaba'
  if (/^glm(?:-|$)/.test(name)) return 'zai'
  if (/^kimi(?:-|$)/.test(name)) return 'moonshotai'
  if (/^minimax(?:-|$)/.test(name)) return 'minimax'
  if (/^mistral(?:-|$)/.test(name)) return 'mistral'
  return undefined
}

/** Select every matching model ID without replacing an existing provider. */
export function selectCatalogResults(current: ReadonlySet<string>, matches: readonly CatalogModel[], catalogByKey: ReadonlyMap<string, CatalogModel>): Set<string> {
  const result = new Set(current)
  const selectedModels = new Set<string>()
  for (const key of current) {
    const item = catalogByKey.get(key)
    if (!item) continue
    const model = item.model_id.toLowerCase()
    if (selectedModels.has(model)) result.delete(key)
    else selectedModels.add(model)
  }
  const choices = new Map<string, CatalogModel>()
  for (const item of matches) {
    const model = item.model_id.toLowerCase()
    if (selectedModels.has(model)) continue
    const previous = choices.get(model)
    const preferred = preferredProvider(model)
    if (!previous || (preferred && item.provider.toLowerCase() === preferred && previous.provider.toLowerCase() !== preferred)) choices.set(model, item)
  }
  for (const item of choices.values()) result.add(item.key)
  return result
}
