import type { DashboardData, GroupRow, SourceId, UsageRecord } from './types'

const now = Math.floor(Date.now() / 1000)
const demoRecords: UsageRecord[] = []

export const sourceMeta: Record<SourceId, { label: string; short: string; accent: string }> = {
  codex: { label: 'Codex', short: 'CX', accent: '#d5f36b' },
  claude: { label: 'Claude Code', short: 'CL', accent: '#d99376' },
  'trae-cn': { label: 'Trae CN', short: 'TC', accent: '#f0a35b' },
  trae: { label: 'Trae 国际版', short: 'TG', accent: '#77d7d0' },
  opencode: { label: 'OpenCode', short: 'OC', accent: '#ae9cff' },
  gemini: { label: 'Gemini CLI', short: 'GE', accent: '#7ea8ff' },
  grokbuild: { label: 'Grok Build', short: 'GB', accent: '#a4a9bc' },
  pi: { label: 'Pi', short: 'PI', accent: '#8fd1dd' },
  omp: { label: 'Oh My Pi', short: 'OP', accent: '#bf9af1' },
  mcode: { label: 'MiniMax Code', short: 'MM', accent: '#ff997e' },
  agy: { label: 'Antigravity', short: 'AG', accent: '#68ceba' },
  kimi: { label: 'Kimi Code', short: 'KI', accent: '#80abed' },
  copilot: { label: 'GitHub Copilot CLI', short: 'GH', accent: '#b0a1eb' },
  workbuddy: { label: 'WorkBuddy', short: 'WB', accent: '#f2bc7f' },
  dsh: { label: 'DeepSeek Harness', short: 'DS', accent: '#86acff' },
}

export const sourceIds = Object.keys(sourceMeta) as SourceId[]

export const emptyStatus = (): DashboardData['source_status'] => Object.fromEntries(sourceIds.map((id) => [id, 'missing'])) as DashboardData['source_status']

export const demoData: DashboardData = {
  records: demoRecords,
  scanned_at: now,
  source_status: { ...emptyStatus(), codex: 'ready', claude: 'ready', 'trae-cn': 'ready', trae: 'ready', opencode: 'ready' },
  source_paths: { ...Object.fromEntries(sourceIds.map((id) => [id, ''])) as Record<SourceId, string>, codex: '~/.codex', claude: '~/.claude/projects', 'trae-cn': '%APPDATA%/Trae CN/.../database.db', trae: '%APPDATA%/Trae/.../database.db', opencode: '~/.local/share/opencode/opencode.db' },
  source_errors: {},
}

export function groupRecords(records: UsageRecord[], field: 'source' | 'project' | 'model' | 'date'): GroupRow[] {
  const groups = new Map<string, GroupRow>()
  for (const record of records) {
    const label = field === 'date'
      ? new Date(record.timestamp * 1000).toISOString().slice(0, 10)
      : field === 'source'
        ? record.source
        : field === 'project'
          ? record.project
          : record.model
    const row = groups.get(label) ?? { label, records: 0, input_tokens: 0, output_tokens: 0, reasoning_tokens: 0, cache_read_tokens: 0, total_tokens: 0 }
    row.records += 1
    row.input_tokens += record.input_tokens
    row.output_tokens += record.output_tokens
    row.reasoning_tokens += record.reasoning_tokens
    row.cache_read_tokens += record.cache_read_tokens
    row.total_tokens += record.total_tokens
    groups.set(label, row)
  }
  return [...groups.values()].sort((a, b) => b.total_tokens - a.total_tokens)
}

export function numericRange(values: Iterable<number>): { min: number; max: number } | null {
  let min = Number.POSITIVE_INFINITY
  let max = Number.NEGATIVE_INFINITY
  let found = false
  for (const value of values) {
    if (value < min) min = value
    if (value > max) max = value
    found = true
  }
  return found ? { min, max } : null
}

export function formatTokens(value: number): string {
  if (value >= 1_000_000_000) return `${(value / 1_000_000_000).toFixed(1)}B`
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`
  if (value >= 1_000) return `${(value / 1_000).toFixed(1)}K`
  return value.toLocaleString('en-US')
}

export function formatExact(value: number): string {
  return value.toLocaleString('en-US')
}
