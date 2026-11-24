export type SourceId = 'codex' | 'claude' | 'trae-cn' | 'trae' | 'opencode' | 'gemini' | 'grokbuild' | 'pi' | 'omp' | 'mcode' | 'agy' | 'kimi' | 'copilot' | 'workbuddy' | 'dsh'

export type UsageRecord = {
  row_id?: number
  source: SourceId
  session_id: string
  timestamp: number
  project: string
  model: string
  input_tokens: number
  output_tokens: number
  reasoning_tokens: number
  cache_read_tokens: number
  cache_write_tokens: number
  total_tokens: number
  cost: number
  cost_status?: 'reported' | 'estimated' | 'unpriced'
}

export type DashboardData = {
  revision?: number
  records: UsageRecord[]
  scanned_at: number
  source_status: Record<SourceId, 'ready' | 'missing' | 'needs-permission' | 'error'>
  source_paths: Record<SourceId, string>
  source_errors: Partial<Record<SourceId, string | null>>
}

export type BackupStatus = { count: number; last_backup_at: number; error: string | null }
export type DashboardStatus = Omit<DashboardData, 'records'> & { revision: number; record_count: number; backup?: BackupStatus }
export type ScanResult = DashboardStatus & { scanned_sources: SourceId[] }

export type UsageSummary = {
  latest_record_id?: number
  revision: number
  record_count: number
  page: number
  totals: { total: number; input: number; output: number; reasoning: number; cache: number; cost: number; priced: number; unpriced: number }
  models: string[]
  projects: string[]
  board_groups: GroupRow[]
  model_groups: GroupRow[]
  trend_records: UsageRecord[]
  calendar_records: UsageRecord[]
  board_totals: number[]
  records: UsageRecord[]
}

export type BoardCardGeometry = { x: number; y: number; width: number; height: number }
export type SharedBoardLayout = { revision: number; updated_at: number; viewport: { width: number; height: number }; cards: BoardCardGeometry[] }
export type BoardWidgetId = 'hour' | 'day' | 'week' | 'month' | 'quarter' | 'year' | 'danmaku'
export type BoardWidgetSettings = { visible: BoardWidgetId[]; order: BoardWidgetId[]; revision: number }

export type DanmakuMessage = { id: string; uid: number; user: string; text: string; timestamp: number; medal: string; medal_level: number; user_level: number }
export type DanmakuState = { room_id: number; real_room_id: number; status: 'not-configured' | 'connecting' | 'connected' | 'reconnecting' | 'disconnected'; authenticated: boolean; font_size: number; error: string | null; revision: number; messages: DanmakuMessage[] }

export type SourceSetting = {
  enabled: boolean
  interval_seconds: number
  last_checked_at: number
  last_changed_at: number
}

export type SourceSettings = Record<SourceId, SourceSetting>

export type SyncSettings = { auto_scan_enabled: boolean; interval_seconds: number }
export type ModelPrice = { model_id: string; name: string; provider: string; input: number; output: number; cache_read: number | null; cache_write: number | null; origin: 'manual' | 'models.dev' }
export type CatalogModel = ModelPrice & { key: string }
export type PricingState = { auto_sync: boolean; selected: string[]; last_synced_at: number; last_error: string | null; prices: ModelPrice[] }

export type GroupRow = {
  label: string
  records: number
  input_tokens: number
  output_tokens: number
  reasoning_tokens: number
  cache_read_tokens: number
  total_tokens: number
}
