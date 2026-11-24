import { t } from './i18n'
import type { BoardCardGeometry, BoardWidgetSettings, CatalogModel, DanmakuState, DashboardData, DashboardStatus, ModelPrice, PricingState, ScanResult, SharedBoardLayout, SourceId, SourceSettings, SyncSettings, UsageSummary } from './types'

async function jsonRequest<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const response = await fetch(path, { method, cache: 'no-store', ...(body === undefined ? {} : { headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) }) })
  if (!response.ok) {
    const data = await response.json().catch(() => ({})) as { error?: string }
    throw new Error(data.error || t('请求失败 ({0})', response.status))
  }
  return response.json() as Promise<T>
}

export const loadSyncSettings = () => jsonRequest<SyncSettings>('/api/sync-settings')
export const saveSyncSettings = (settings: SyncSettings) => jsonRequest<SyncSettings>('/api/sync-settings', 'PUT', settings)
export const signalUsagePresence = (opened: boolean) => jsonRequest<{ ok: boolean }>('/api/sync/heartbeat', 'POST', { opened })
export const loadPricing = () => jsonRequest<PricingState>('/api/pricing')
export const loadPricingCatalog = () => jsonRequest<{ models: CatalogModel[] }>('/api/pricing/catalog')
export const savePricingSettings = (value: Partial<Pick<PricingState, 'auto_sync' | 'selected'>>) => jsonRequest<PricingState>('/api/pricing/settings', 'PUT', value)
export const saveModelPrice = (value: Omit<ModelPrice, 'origin'>) => jsonRequest<PricingState>('/api/pricing/entry', 'PUT', value)
export const deleteModelPrice = (modelId: string) => jsonRequest<PricingState>('/api/pricing/entry', 'DELETE', { model_id: modelId })
export const syncModelPrices = () => jsonRequest<{ updated: number; preserved: number; state: PricingState }>('/api/pricing/sync', 'POST', {})

export async function loadDashboard(): Promise<DashboardData> {
  const response = await fetch('/api/dashboard')
  if (!response.ok) throw new Error(`dashboard request failed (${response.status})`)
  return response.json() as Promise<DashboardData>
}

export async function loadDashboardStatus(): Promise<DashboardStatus> {
  const response = await fetch('/api/dashboard/status', { cache: 'no-store' })
  if (!response.ok) throw new Error(`dashboard status request failed (${response.status})`)
  return response.json() as Promise<DashboardStatus>
}

export type UsageQuery = { source: string; model: string; project: string; start: number | null; end: number | null; now: number; includeBoard: boolean; query: string; page: number; hidden: SourceId[]; revision: number }
export async function loadUsageSummary(filters: UsageQuery, signal?: AbortSignal): Promise<UsageSummary> {
  const params = new URLSearchParams({ source: filters.source, model: filters.model, project: filters.project, now: String(filters.now), query: filters.query, page: String(filters.page), page_size: '20' })
  if (!filters.includeBoard) params.set('include_board', '0')
  if (filters.start !== null) params.set('start', String(filters.start))
  if (filters.end !== null) params.set('end', String(filters.end))
  for (const source of filters.hidden) params.append('hidden', source)
  const response = await fetch(`/api/usage?${params}`, { cache: 'no-store', signal })
  if (response.status === 404 || (response.ok && !response.headers.get('content-type')?.includes('application/json'))) {
    const { loadLegacyUsageSummary } = await import('./legacyUsage')
    return loadLegacyUsageSummary(filters, signal)
  }
  if (!response.ok) throw new Error(`usage request failed (${response.status})`)
  return response.json() as Promise<UsageSummary>
}

export async function loadSourceSettings(): Promise<SourceSettings> {
  const response = await fetch('/api/source-settings', { cache: 'no-store' })
  if (!response.ok) throw new Error(`source settings request failed (${response.status})`)
  return response.json() as Promise<SourceSettings>
}

export async function saveSourceSettings(settings: SourceSettings): Promise<SourceSettings> {
  const response = await fetch('/api/source-settings', {
    method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(settings),
  })
  if (!response.ok) throw new Error(`source settings update failed (${response.status})`)
  return response.json() as Promise<SourceSettings>
}

export async function scanUsageSources(sources: SourceId[], force = true): Promise<ScanResult> {
  return jsonRequest<ScanResult>('/api/scan', 'POST', { sources, force })
}

export async function rebuildCodexUsage(): Promise<ScanResult> {
  const response = await fetch('/api/codex/rebuild', {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}',
  })
  if (!response.ok) throw new Error(`Codex rebuild failed (${response.status})`)
  return response.json() as Promise<ScanResult>
}

export async function loadBoardLayout(): Promise<SharedBoardLayout> {
  const response = await fetch('/api/board-layout', { cache: 'no-store' })
  if (!response.ok) throw new Error(`board layout request failed (${response.status})`)
  return response.json() as Promise<SharedBoardLayout>
}

export async function saveBoardLayout(viewport: { width: number; height: number }, cards: BoardCardGeometry[]): Promise<SharedBoardLayout> {
  const response = await fetch('/api/board-layout', {
    method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ viewport, cards }),
  })
  if (!response.ok) throw new Error(`board layout update failed (${response.status})`)
  return response.json() as Promise<SharedBoardLayout>
}

export async function loadBoardSettings(): Promise<BoardWidgetSettings> {
  const response = await fetch('/api/board-settings', { cache: 'no-store' })
  if (!response.ok) throw new Error(`board settings request failed (${response.status})`)
  return response.json() as Promise<BoardWidgetSettings>
}

export async function saveBoardSettings(settings: Pick<BoardWidgetSettings, 'visible' | 'order'>): Promise<BoardWidgetSettings> {
  const response = await fetch('/api/board-settings', {
    method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(settings),
  })
  if (!response.ok) throw new Error(`board settings update failed (${response.status})`)
  return response.json() as Promise<BoardWidgetSettings>
}

export async function loadDanmakuState(): Promise<DanmakuState> {
  const response = await fetch('/api/danmaku', { cache: 'no-store' })
  if (!response.ok) throw new Error(`danmaku request failed (${response.status})`)
  return response.json() as Promise<DanmakuState>
}

export async function saveDanmakuSettings(roomId: number, cookie?: string, clearCookie = false, fontSize?: number): Promise<DanmakuState> {
  const response = await fetch('/api/danmaku/settings', {
    method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ room_id: roomId, ...(cookie ? { cookie } : {}), clear_cookie: clearCookie, ...(fontSize !== undefined ? { font_size: fontSize } : {}) }),
  })
  if (!response.ok) {
    const payload = await response.json().catch(() => ({})) as { error?: string }
    throw new Error(payload.error || `danmaku settings update failed (${response.status})`)
  }
  return response.json() as Promise<DanmakuState>
}

export async function exportUsage(format: 'csv' | 'json'): Promise<string> {
  const response = await fetch(`/api/export?format=${encodeURIComponent(format)}`)
  if (!response.ok) throw new Error(`export request failed (${response.status})`)
  const blob = await response.blob()
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = `tokenusage-${new Date().toISOString().slice(0, 10)}.${format}`
  link.click()
  URL.revokeObjectURL(url)
  return link.download
}
