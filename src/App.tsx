import { useBoardPopoverDismiss } from './useBoardPopoverDismiss'
import { lazy, startTransition, Suspense, useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode } from 'react'
import {
  Activity, ArrowDownToLine, BarChart3, CalendarDays, ChartCandlestick, ChevronDown,
  Copy, Database, Download, Gauge, GripVertical, Languages, Layers3, LayoutDashboard, Menu, Magnet, Search,
  Settings2,
} from 'lucide-react'
import { exportUsage, loadBoardLayout, loadBoardSettings, loadDanmakuState, loadDashboardStatus, loadPricing, loadSourceSettings, loadSyncSettings, loadUsageSummary, rebuildCodexUsage, saveBoardLayout, saveBoardSettings, saveSyncSettings, scanUsageSources, signalUsagePresence } from './api'
import { demoData, emptyStatus, formatExact, formatTokens, numericRange, sourceIds, sourceMeta } from './data'
import { resolveTimeWindow } from './usageTrend'
import { arrivingLedgerRows } from './ledgerRows'
import { formatCostDetail } from './axisFormat'
import CustomRangePopover from './CustomRangePopover'
import Dropdown from './Dropdown'
import DanmakuCard, { DanmakuToolbar } from './DanmakuCard'
import { defaultBoardCards } from './boardLayout'
import { compileTokenBoardCss, TokenBoardStyleToolbar, useTokenBoardStyle } from './TokenBoardStyles'
import SyncSettingsPanel from './SyncSettingsPanel'
import PricingDialog from './PricingDialog'
import { locale, setLanguage, t, useLanguage } from './i18n'
import './sync-pricing.css'
import type { TimeRange, TimeWindow } from './usageTrend'
import type { BoardCardGeometry, BoardWidgetId, BoardWidgetSettings, DanmakuState, DashboardData, DashboardStatus, GroupRow, PricingState, SharedBoardLayout, SourceId, SourceSettings, SyncSettings, UsageRecord, UsageSummary } from './types'

type ViewId = 'overview' | 'board-preview' | 'board'
type UsageFilters = { source: SourceId | 'all'; range: TimeRange; model: string; project: string; customStart: number | null; customEnd: number | null; liveEnd: boolean }
const HourlyUsageLinePanel = lazy(() => import('./HourlyUsageLinePanel'))
const TokenCalendarHeatmap = lazy(() => import('./TokenCalendarHeatmap'))
const DASHBOARD_POLL_MS = 5_000
const OBS_BOARD_PATH = '/?view=board'
const BOARD_SNAP_KEY = 'tokenusage.board.autoSnap'
const defaultSourceSettings = (): SourceSettings => Object.fromEntries(sourceIds.map((source) => [source, {
  enabled: true, interval_seconds: 60, last_checked_at: 0, last_changed_at: 0,
}])) as SourceSettings

const timeRangeOptions: { value: TimeRange; label: string }[] = [
  { value: 'hour', label: '1小时' },
  { value: 'day', label: '1天' },
  { value: 'week', label: '7天' },
  { value: 'month', label: '30天' },
  { value: 'all', label: '全部' },
  { value: 'custom', label: '自定义' },
]

const navItems: { id: ViewId; label: string; icon: typeof Gauge }[] = [
  { id: 'overview', label: '总览', icon: Gauge },
  { id: 'board-preview', label: '看板', icon: LayoutDashboard },
]
const boardWidgetOptions: { id: BoardWidgetId; label: string; description: string }[] = [
  { id: 'hour', label: '1小时 Token 消耗', description: '最近 1 小时累计 Token' },
  { id: 'day', label: '1天 Token 消耗', description: '最近 24 小时累计 Token' },
  { id: 'week', label: '7天 Token 消耗', description: '最近 7 天累计 Token' },
  { id: 'month', label: '30天 Token 消耗', description: '最近 30 天累计 Token' },
  { id: 'quarter', label: '90天 Token 消耗', description: '最近 90 天累计 Token' },
  { id: 'year', label: '1年 Token 消耗', description: '最近 365 天累计 Token' },
  { id: 'danmaku', label: '弹幕姬', description: 'B站实时弹幕卡片' },
]
const boardWidgetOrder = boardWidgetOptions.map(({ id }) => id)

function initialView(): ViewId {
  const candidate = new URLSearchParams(window.location.search).get('view')
  if (candidate === 'settings') return 'board-preview'
  return candidate === 'board' || navItems.some((item) => item.id === candidate) ? candidate as ViewId : 'overview'
}

function App() {
  const language = useLanguage()
  const [view, setView] = useState<ViewId>(initialView)
  useEffect(() => {
    const url = new URL(window.location.href)
    if (url.searchParams.get('view') !== 'sources') return
    url.searchParams.delete('view')
    window.history.replaceState(null, '', url)
  }, [])
  const [autoSnap, setAutoSnap] = useState(() => {
    try { return window.localStorage.getItem(BOARD_SNAP_KEY) !== 'off' } catch { return true }
  })
  const toggleAutoSnap = () => {
    const next = !autoSnap
    setAutoSnap(next)
    try { window.localStorage.setItem(BOARD_SNAP_KEY, next ? 'on' : 'off') } catch { /* current page still updates */ }
  }
  const [data, setData] = useState<DashboardStatus>(() => ({ ...demoData, revision: 0, record_count: 0, source_status: emptyStatus(), source_errors: {} }))
  const [usageWindow, setUsageWindow] = useState<{ scope: string | null; summary: UsageSummary; arrivals: ReadonlySet<number> } | null>(null)
  const usage = usageWindow?.summary ?? null
  const [loading, setLoading] = useState(false)
  const [notice, setNotice] = useState('')
  const [query, setQuery] = useState('')
  const [settledQuery, setSettledQuery] = useState('')
  const [mobileNav, setMobileNav] = useState(false)
  const [filters, setFilters] = useState<UsageFilters>({ source: 'all', range: 'all', model: 'all', project: 'all', customStart: null, customEnd: null, liveEnd: false })
  const [rangeClock, setRangeClock] = useState(() => Math.floor(Date.now() / 1000))
  const [sourceSettings, setSourceSettings] = useState<SourceSettings>(defaultSourceSettings)
  const [syncSettings, setSyncSettings] = useState<SyncSettings>({ auto_scan_enabled: true, interval_seconds: 60 })
  const [pricing, setPricing] = useState<PricingState>({ auto_sync: false, selected: [], last_synced_at: 0, last_error: null, prices: [] })
  const [syncOpen, setSyncOpen] = useState(false)
  const [priceRevision, setPriceRevision] = useState(0)
  const priceSignature = useRef<string | null>(null)
  const [pricingOpen, setPricingOpen] = useState(false)
  const [page, setPage] = useState(1)
  const dashboardLoadInFlight = useRef(false)
  const scanInFlight = useRef(false)
  const syncSettingsGeneration = useRef(0)
  const syncSettingsSaving = useRef(false)
  const revisionRef = useRef(0)

  const applyDashboard = useCallback((next: DashboardStatus) => {
    const nextRevision = Number(next.revision ?? next.scanned_at * 1000)
    if (nextRevision < revisionRef.current) return
    revisionRef.current = nextRevision
    setRangeClock(Math.floor(Date.now() / 1000))
    startTransition(() => setData(next))
  }, [])

  const refresh = useCallback(async (force = false) => {
    if (dashboardLoadInFlight.current) return
    dashboardLoadInFlight.current = true
    try {
      const status = await loadDashboardStatus()
      if (!force && status.revision === revisionRef.current) {
        setData((current) => JSON.stringify(current.backup) === JSON.stringify(status.backup) ? current : { ...current, backup: status.backup })
        return
      }
      applyDashboard(status)
      setNotice((current) => current === t('本地网页采集服务暂不可用') ? '' : current)
    } catch {
      setNotice(t('本地网页采集服务暂不可用'))
    } finally {
      dashboardLoadInFlight.current = false
    }
  }, [applyDashboard])

  const applyPricing = useCallback((next: PricingState) => {
    const signature = JSON.stringify(next.prices)
    const previous = priceSignature.current
    priceSignature.current = signature
    // The first usage response already reads server prices. Do not cancel it
    // just because the settings request finished loading the same prices.
    if (previous !== null && previous !== signature) setPriceRevision((revision) => revision + 1)
    setPricing(next)
  }, [])

  useEffect(() => {
    void refresh(true)
    void loadSourceSettings().then(setSourceSettings).catch(() => undefined)
    void loadSyncSettings().then(setSyncSettings).catch(() => undefined)
    void loadPricing().then(applyPricing).catch(() => undefined)
  }, [refresh, applyPricing])
  useEffect(() => {
    const timer = window.setInterval(() => {
      void refresh()
      const syncGeneration = syncSettingsGeneration.current
      void loadSourceSettings().then(setSourceSettings).catch(() => undefined)
      void loadSyncSettings().then((next) => { if (!syncSettingsSaving.current && syncGeneration === syncSettingsGeneration.current) setSyncSettings(next) }).catch(() => undefined)
    }, DASHBOARD_POLL_MS)
    return () => window.clearInterval(timer)
  }, [refresh])
  useEffect(() => {
    if (pricingOpen) return
    const timer = window.setInterval(() => void loadPricing().then(applyPricing).catch(() => undefined), 30_000)
    return () => window.clearInterval(timer)
  }, [pricingOpen, applyPricing])
  useEffect(() => {
    const timer = window.setTimeout(() => setSettledQuery(query), 250)
    return () => window.clearTimeout(timer)
  }, [query])
  useEffect(() => {
    if (view !== 'overview') return
    const announce = (opened: boolean) => { if (document.visibilityState === 'visible') void signalUsagePresence(opened).catch(() => undefined) }
    const onVisibility = () => announce(true)
    announce(true)
    document.addEventListener('visibilitychange', onVisibility)
    const timer = window.setInterval(() => announce(false), 15_000)
    return () => { window.clearInterval(timer); document.removeEventListener('visibilitychange', onVisibility) }
  }, [view])
  useEffect(() => {
    if (filters.range === 'all' || (filters.range === 'custom' && !filters.liveEnd && filters.customEnd !== null)) return
    setRangeClock(Math.floor(Date.now() / 1000))
    const timer = window.setInterval(() => setRangeClock(Math.floor(Date.now() / 1000)), 30_000)
    return () => window.clearInterval(timer)
  }, [filters.range, filters.liveEnd, filters.customEnd])
  useEffect(() => {
    document.documentElement.classList.toggle('obs-board-mode', view === 'board')
    return () => document.documentElement.classList.remove('obs-board-mode')
  }, [view])
  useEffect(() => {
    if (view !== 'board' || window.parent !== window) return
    const leaveBoard = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      setView('overview')
      const url = new URL(window.location.href)
      url.searchParams.delete('view')
      window.history.replaceState(null, '', url)
    }
    window.addEventListener('keydown', leaveBoard)
    return () => window.removeEventListener('keydown', leaveBoard)
  }, [view])

  const scan = useCallback(async (sources: SourceId[]) => {
    if (scanInFlight.current || !sources.length) return
    scanInFlight.current = true
    setLoading(true)
    setNotice(t('正在扫描 {0}', sources.map((source) => t(sourceMeta[source].label)).join('、')))
    try {
      const result = await scanUsageSources(sources, false)
      applyDashboard(result)
      setSourceSettings(await loadSourceSettings())
      const failed = sources.filter((source) => result.source_status[source] === 'error' || result.source_status[source] === 'needs-permission')
      setNotice(failed.length ? t('同步完成，{0}需要检查', failed.map((source) => t(sourceMeta[source].label)).join('、')) : t('同步完成 · {0}', new Date().toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' })))
    } catch (error) {
      setNotice(error instanceof Error ? error.message : t('同步失败，请查看数据源状态'))
    } finally {
      setLoading(false)
      scanInFlight.current = false
    }
  }, [applyDashboard])

  const updateSyncSettings = useCallback(async (next: SyncSettings) => {
    syncSettingsGeneration.current += 1
    syncSettingsSaving.current = true
    setSyncSettings(next)
    try { setSyncSettings(await saveSyncSettings(next)); setNotice(t('同步设置已保存')) }
    catch (error) { setNotice(error instanceof Error ? error.message : t('同步设置保存失败')); try { setSyncSettings(await loadSyncSettings()) } catch { /* retain visible state */ } }
    finally { syncSettingsSaving.current = false }
  }, [])

  const rebuildCodex = useCallback(async () => {
    if (scanInFlight.current) return
    scanInFlight.current = true
    setLoading(true)
    setNotice(t('正在使用项目内置的 CCSwitch 算法重建 Codex 用量'))
    try {
      const result = await rebuildCodexUsage()
      applyDashboard(result)
      setSourceSettings(await loadSourceSettings())
      setNotice(t('Codex 用量重建完成 · {0}', new Date().toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' })))
    } catch {
      setNotice(t('Codex 用量重建失败'))
    } finally {
      setLoading(false)
      scanInFlight.current = false
    }
  }, [applyDashboard])

  const timeWindow = useMemo(() => resolveTimeWindow(filters.range, filters.customStart, filters.customEnd, filters.liveEnd, Math.max(rangeClock, Math.floor(Date.now() / 1000))), [filters.range, filters.customStart, filters.customEnd, filters.liveEnd, rangeClock])
  const ledgerScope = view === 'overview' && page === 1 ? JSON.stringify([filters, settledQuery]) : null
  useEffect(() => { setPage(1) }, [filters, settledQuery])
  useEffect(() => {
    if (view === 'board-preview') {
      setUsageWindow((current) => current?.scope != null ? { ...current, scope: null, arrivals: new Set() } : current)
      return
    }
    if (!data.revision) return
    const controller = new AbortController()
    void loadUsageSummary({ source: filters.source, model: filters.model, project: filters.project, start: timeWindow.start, end: timeWindow.end, now: rangeClock, includeBoard: view === 'board', query: settledQuery, page, hidden: [], revision: data.revision }, controller.signal)
      .then((next) => {
        if (controller.signal.aborted) return
        startTransition(() => setUsageWindow((current) => ({
          scope: ledgerScope, summary: next, arrivals: arrivingLedgerRows(current, ledgerScope, next),
        })))
      })
      .catch(() => { if (!controller.signal.aborted) setNotice(t('用量数据查询失败')) })
    return () => controller.abort()
  }, [data.revision, view, filters.source, filters.model, filters.project, timeWindow.start, timeWindow.end, rangeClock, settledQuery, page, priceRevision, ledgerScope])
  const modelOptions = usage?.models ?? []
  const projectOptions = usage?.projects ?? []
  const recordCount = usage?.record_count ?? 0
  const pageCount = Math.max(1, Math.ceil(recordCount / 20))
  const pagedRecords = usage?.records ?? []

  const totals = usage?.totals ?? null

  const selectView = (next: ViewId) => {
    setView(next)
    setMobileNav(false)
    const url = new URL(window.location.href)
    if (next === 'overview') url.searchParams.delete('view')
    else url.searchParams.set('view', next)
    window.history.replaceState(null, '', url)
  }
  const handleExport = async (format: 'csv' | 'json') => {
    try {
      const path = await exportUsage(format)
      setNotice(t('已下载 {0}', path))
    } catch { setNotice(t('导出失败，请检查写入权限')) }
  }

  if (view === 'board') {
    return <main className="obs-board-root"><BoardDashboard totals={usage?.board_totals ?? []} editable={window.frameElement?.classList.contains('board-preview-frame') === true} /></main>
  }

  return (
    <div className="app-shell">
      <aside className={`sidebar ${mobileNav ? 'sidebar-open' : ''}`}>
        <div className="brand-lockup">
          <div className="brand-mark"><Activity size={18} strokeWidth={2.4} /></div>
          <strong>TokenUsage</strong>
        </div>
        <nav>
          {navItems.map(({ id, label, icon: Icon }) => (
            <button key={id} className={`nav-item ${view === id ? 'active' : ''}`} onClick={() => selectView(id)} aria-current={view === id ? 'page' : undefined}>
              <Icon size={17} /><span>{t(label)}</span>{id === 'overview' && <span className="nav-ping" />}
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          <div className="language-control">
            <span><Languages aria-hidden="true" size={15} />{t('语言')}</span>
            <Dropdown label={t('语言')} value={language}
              options={[{ value: 'zh-CN', label: '简体中文', lang: 'zh-CN' }, { value: 'en', label: 'English', lang: 'en' }]}
              onChange={(value) => { setNotice(''); setLanguage(value === 'en' ? 'en' : 'zh-CN') }} />
          </div>
        </div>
      </aside>

      <main className={view === 'board-preview' ? 'main-content main-content-board' : 'main-content'}>
        <header className={view === 'overview' ? 'topbar topbar-overview' : 'topbar'}>
          <button className="mobile-menu" onClick={() => setMobileNav((open) => !open)} aria-label={t('打开导航')} aria-expanded={mobileNav}><Menu size={20} /></button>
          <div className="topbar-actions">
            {view === 'board-preview' && <BoardToolbar autoSnap={autoSnap} onToggleSnap={toggleAutoSnap} />}
          </div>
        </header>

        {view === 'board-preview' ? <iframe className="board-preview-frame" title={t('OBS 看板')} src={OBS_BOARD_PATH} data-board-snap={autoSnap ? 'on' : 'off'} /> : <div className="page-wrap">
          <section className="page-heading">
            <div>
              <h1>{t('用量总览')}</h1>
              {notice && !syncOpen && <p role="status">{notice}</p>}
            </div>
            <div className="heading-actions">
              <button className="button button-quiet" onClick={() => handleExport('csv')}><Download size={15} />{t('导出 CSV')}</button>
              <button className="button button-quiet" onClick={() => setSyncOpen(true)}><Settings2 size={16} />{t('用量设置')}</button>
            </div>
          </section>

          {view === 'overview' && <Overview totals={totals} trendRecords={usage?.trend_records ?? []} calendarRecords={usage?.calendar_records ?? []} boardRows={usage?.board_groups ?? []} modelRows={usage?.model_groups ?? []} timeWindow={timeWindow} filters={filters} modelOptions={modelOptions} projectOptions={projectOptions} onFiltersChange={setFilters} />}

          <section className="ledger-section">
            <div className="section-heading">
              <div><h2>{t('明细记录')}</h2></div>
              <div className="table-tools">
                <div className="search-box"><Search size={15} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={t('搜索来源、模型、项目')} /></div>
              </div>
            </div>
            <div className="ledger-table-wrap">
              <table className="ledger-table"><thead><tr><th>{t('来源')}</th><th>{t('模型')}</th><th>{t('项目')}</th><th>{t('时间')}</th><th className="numeric">{t('输入')}</th><th className="numeric">{t('输出')}</th><th className="numeric">{t('总量')}</th><th className="numeric">{t('成本')}</th></tr></thead>
                <tbody>{pagedRecords.map((record, index) => <tr key={record.row_id ?? `${record.source}-${record.session_id}-${record.timestamp}-${index}`} className={record.row_id !== undefined && usageWindow?.arrivals.has(record.row_id) ? 'ledger-row-new' : undefined}><td><SourceBadge source={record.source} /></td><td className="model-cell">{record.model}</td><td className="project-cell">{record.project}</td><td className="muted-cell">{new Date(record.timestamp * 1000).toLocaleDateString(locale())}</td><td className="numeric">{formatTokens(record.input_tokens)}</td><td className="numeric">{formatTokens(record.output_tokens)}</td><td className="numeric total-cell">{formatTokens(record.total_tokens)}</td><td className="numeric" title={record.cost_status === 'estimated' ? t('按当前模型价格估算') : record.cost > 0 ? t('来源记录的成本') : t('缺少定价')}>{record.cost_status === 'estimated' ? `≈${formatCostDetail(record.cost)}` : record.cost > 0 ? formatCostDetail(record.cost) : t('未定价')}</td></tr>)}</tbody>
              </table>
              {usage && !recordCount && <div className="empty-state"><Database size={22} />{t('没有匹配的记录')}</div>}
            </div>
            <div className="pagination"><span>{t('{0} 条记录 · 第 {1}/{2} 页', recordCount.toLocaleString(locale()), Math.min(page, pageCount), pageCount)}</span><div><button disabled={page <= 1} onClick={() => setPage((value) => Math.max(1, value - 1))}>{t('上一页')}</button><button disabled={page >= pageCount} onClick={() => setPage((value) => Math.min(pageCount, value + 1))}>{t('下一页')}</button></div></div>
          </section>

        </div>}
      </main>
      {syncOpen && <SyncSettingsPanel sync={syncSettings} sources={sourceSettings} statuses={data.source_status} errors={data.source_errors} backup={data.backup} loading={loading} message={notice} onUpdate={(value) => void updateSyncSettings(value)} onSync={() => void scan(sourceIds)} onRebuild={() => void rebuildCodex()} onPricing={() => { setSyncOpen(false); setPricingOpen(true) }} onClose={() => setSyncOpen(false)} />}
      {pricingOpen && <PricingDialog state={pricing} usedModels={modelOptions} onChange={applyPricing} onClose={() => setPricingOpen(false)} />}
    </div>
  )
}

function BoardToolbar({ autoSnap, onToggleSnap }: { autoSnap: boolean; onToggleSnap: () => void }) {
  const [copied, setCopied] = useState(false)
  const obsUrl = new URL(OBS_BOARD_PATH, window.location.origin).href
  const copyAddress = async () => {
    try {
      await navigator.clipboard.writeText(obsUrl)
      setCopied(true)
    } catch {
      window.prompt(t('请手动复制浏览器地址'), obsUrl)
    }
  }
  return <div className="board-toolbar">
    <button type="button" className="button button-quiet board-snap-toggle" aria-pressed={autoSnap} onClick={onToggleSnap} title={t('移动或缩放卡片时，与其他卡片的边缘相差 5px 以内自动对齐')}><Magnet size={15} /><span>{t('自动对齐')}</span></button>
    <button type="button" className="button button-quiet" aria-label={t('复制浏览器地址')} title={obsUrl} onClick={() => void copyAddress()}><Copy size={15} /><span aria-live="polite">{copied ? t('地址已复制') : t('复制浏览器地址')}</span></button>
    <BoardSourcesToolbar />
    <TokenBoardStyleToolbar />
    <DanmakuToolbar />
  </div>
}

function Overview({ totals, trendRecords, calendarRecords, boardRows, modelRows, timeWindow, filters, modelOptions, projectOptions, onFiltersChange }: { totals: UsageSummary['totals'] | null; trendRecords: UsageRecord[]; calendarRecords: UsageRecord[]; boardRows: GroupRow[]; modelRows: GroupRow[]; timeWindow: TimeWindow; filters: UsageFilters; modelOptions: string[]; projectOptions: string[]; onFiltersChange: (next: UsageFilters) => void }) {
  const [visual, setVisual] = useState<'trend' | 'calendar' | 'boards' | 'models'>(() => {
    const oldView = new URLSearchParams(window.location.search).get('view')
    return oldView === 'models' ? 'models' : oldView === 'projects' ? 'boards' : 'trend'
  })
  const [customOpen, setCustomOpen] = useState(false)
  const rangeAnchorRef = useRef<HTMLDivElement>(null)
  const customButtonRef = useRef<HTMLButtonElement>(null)
  const activeVisual = visual === 'calendar' && filters.range !== 'all' ? 'trend' : visual
  useEffect(() => {
    if (filters.range !== 'all') setVisual((current) => current === 'calendar' ? 'trend' : current)
  }, [filters.range])
  const closeCustom = (restoreFocus = false) => {
    setCustomOpen(false)
    if (restoreFocus) customButtonRef.current?.focus()
  }
  return <>
    <section className="overview-controls">
      <div className={`range-control ${customOpen ? 'range-control-open' : ''}`} ref={rangeAnchorRef}>
        <div className="range-tabs" aria-label={t('时间范围')}>{timeRangeOptions.map((option) => <button key={option.value} ref={option.value === 'custom' ? customButtonRef : undefined} className={filters.range === option.value ? 'selected' : ''} aria-pressed={filters.range === option.value} aria-haspopup={option.value === 'custom' ? 'dialog' : undefined} aria-expanded={option.value === 'custom' ? customOpen : undefined} aria-controls={option.value === 'custom' && customOpen ? 'custom-range-popover' : undefined} onClick={() => {
          if (option.value === 'custom') setCustomOpen((open) => !open)
          else { setCustomOpen(false); onFiltersChange({ ...filters, range: option.value }) }
        }}>{t(option.label)}</button>)}</div>
        {customOpen && <CustomRangePopover value={{ start: filters.customStart, end: filters.customEnd, liveEnd: filters.liveEnd }} anchorRef={rangeAnchorRef} onClose={closeCustom} onApply={(value) => { onFiltersChange({ ...filters, range: 'custom', customStart: value.start, customEnd: value.end, liveEnd: value.liveEnd }); closeCustom(true) }} />}
      </div>
      <div className="overview-filter-pills">
        <Dropdown variant="pill" label={t('终端')} value="all" onChange={() => undefined}
          options={[{ value: 'all', label: t('全部终端') }, { value: 'desktop', label: t('桌面端') }]} />
        <Dropdown variant="pill" label={t('工具')} value={filters.source} onChange={(value) => onFiltersChange({ ...filters, source: value as UsageFilters['source'] })}
          options={[{ value: 'all', label: t('全部工具') }, ...sourceIds.map((source) => ({ value: source, label: t(sourceMeta[source].label) }))]} />
        <Dropdown variant="pill" label={t('模型')} value={filters.model} searchPlaceholder={t('搜索模型…')}
          onChange={(model) => onFiltersChange({ ...filters, model })}
          options={[{ value: 'all', label: t('全部模型') }, ...modelOptions.filter((value) => value !== 'all').map((value) => ({ value, label: value }))]} />
        <Dropdown variant="pill" label={t('项目')} value={filters.project} searchPlaceholder={t('搜索项目…')}
          onChange={(project) => onFiltersChange({ ...filters, project })}
          options={[{ value: 'all', label: t('全部项目') }, ...projectOptions.filter((value) => value !== 'all').map((value) => ({ value, label: value }))]} />
      </div>
    </section>
    <section className="metric-grid" aria-busy={totals === null}>
      <MetricCard label={t('累计 Token')} value={totals ? formatExact(totals.total) : '—'} tone="lime" icon={<Gauge size={17} />} />
      <MetricCard label={t('输入 Token')} value={totals ? formatTokens(totals.input) : '—'} exactValue={totals ? formatExact(totals.input) : undefined} tone="orange" icon={<ArrowDownToLine size={17} />} />
      <MetricCard label={t('输出 Token')} value={totals ? formatTokens(totals.output) : '—'} exactValue={totals ? formatExact(totals.output) : undefined} tone="teal" icon={<BarChart3 size={17} />} />
      <MetricCard label={t('缓存读取')} value={totals ? formatTokens(totals.cache) : '—'} exactValue={totals ? formatExact(totals.cache) : undefined} tone="violet" icon={<Database size={17} />} />
    </section>
    <div className="estimated-cost-strip"><div><span>{t('已计价成本')}</span><strong>{!totals ? '—' : totals.priced ? formatCostDetail(totals.cost) : t('暂无定价')}</strong>{totals && <small>{totals.unpriced ? t('{0} 条记录未计价', totals.unpriced.toLocaleString()) : t('按当前价格估算')}</small>}</div></div>
    <section className="panel overview-visual-module">
      <div className="overview-visual-header">
        <div><h2>{t('用量视图')}</h2></div>
        <div className="overview-visual-tabs" role="group" aria-label={t('切换用量视图')}>
          <button className={activeVisual === 'trend' ? 'selected' : ''} aria-pressed={activeVisual === 'trend'} onClick={() => setVisual('trend')}><Activity size={15} />{t('用量趋势')}</button>
          <button className={activeVisual === 'calendar' ? 'selected' : ''} aria-pressed={activeVisual === 'calendar'} disabled={filters.range !== 'all'} title={filters.range !== 'all' ? t('选择“全部”时间范围后可查看') : t('查看活跃日历')} onClick={() => setVisual('calendar')}><CalendarDays size={15} />{t('活跃日历')}</button>
          <button className={activeVisual === 'boards' ? 'selected' : ''} aria-pressed={activeVisual === 'boards'} onClick={() => setVisual('boards')}><Layers3 size={15} />{t('看板分类')}</button>
          <button className={activeVisual === 'models' ? 'selected' : ''} aria-pressed={activeVisual === 'models'} onClick={() => setVisual('models')}><BarChart3 size={15} />{t('模型消耗排行')}</button>
        </div>
      </div>
      <div className="overview-visual-content" aria-busy={totals === null}>
        {!totals && <section className="panel hourly-line-panel line-chart-loading">{t('正在加载用量…')}</section>}
        {filters.range !== 'all' && <p className="overview-calendar-note">{t('活跃日历在选择“全部”时间范围后可查看')}</p>}
        {totals && activeVisual === 'trend' && <Suspense fallback={<section className="panel hourly-line-panel line-chart-loading">{t('正在绘制用量趋势…')}</section>}><HourlyUsageLinePanel records={trendRecords} timeWindow={timeWindow} /></Suspense>}
        {totals && activeVisual === 'calendar' && <Suspense fallback={<section className="panel token-calendar-panel line-chart-loading">{t('正在生成 Token 日历…')}</section>}><TokenCalendarHeatmap records={calendarRecords} /></Suspense>}
        {totals && activeVisual === 'boards' && <BoardCategories rows={boardRows} total={totals.total} />}
        {totals && activeVisual === 'models' && <ModelRanking rows={modelRows} />}
      </div>
    </section>
  </>
}

function BoardCategories({ rows, total }: { rows: GroupRow[]; total: number }) {
  const max = Math.max(rows[0]?.total_tokens ?? 1, 1)
  return <section className="board-panel panel" aria-label={t('看板分类')}>
    <div className="overview-visual-meta"><span className="panel-count">{t('{0} 个看板', rows.length)}</span></div>
    {rows.length ? <div className="board-list" role="region" aria-label={t('看板分类列表')} tabIndex={0}>{rows.map((row, index) => <div className="board-row" key={row.label}><span className="board-index">{String(index + 1).padStart(2, '0')}</span><div className="board-icon">{row.label.slice(0, 1).toUpperCase()}</div><div className="board-main"><div className="board-title"><strong>{row.label || t('未命名看板')}</strong><span>{t('{0} 条记录', row.records.toLocaleString(locale()))}</span></div><div className="board-track"><i style={{ width: `${(row.total_tokens / max) * 100}%` }} /></div></div><div className="board-total"><strong>{formatTokens(row.total_tokens)}</strong><small>{total ? `${((row.total_tokens / total) * 100).toFixed(1)}%` : '0.0%'}</small></div></div>)}</div> : <div className="empty-state">{t('暂无看板分类')}</div>}
  </section>
}

type Candle = { label: string; open: number; high: number; low: number; close: number; volume: number; count?: number }

function UsageCandlePanel({ records, range }: { records: DashboardData['records']; range: TimeRange }) {
  const candles = useMemo<Candle[]>(() => {
    if (!records.length) return []
    const sorted = [...records].sort((a, b) => a.timestamp - b.timestamp)
    const start = sorted[0].timestamp
    const end = sorted[sorted.length - 1].timestamp
    const span = Math.max(end - start, 1)
    const bucketSize = range === 'hour' ? 5 * 60 : range === 'day' ? 2 * 60 * 60 : range === 'week' ? 24 * 60 * 60 : range === 'month' ? 3 * 24 * 60 * 60 : Math.max(24 * 60 * 60, Math.ceil(span / 12))
    const buckets = new Map<number, DashboardData['records']>()
    for (const record of sorted) {
      const key = Math.floor(record.timestamp / bucketSize) * bucketSize
      const bucket = buckets.get(key) ?? []
      bucket.push(record)
      buckets.set(key, bucket)
    }
    return [...buckets.entries()].slice(-12).map(([key, bucket]) => {
      const values = bucket.map((item) => item.total_tokens)
      const range = numericRange(values)!
      return {
        label: new Date(key * 1000).toLocaleDateString(locale(), { month: 'numeric', day: 'numeric' }),
        open: values[0], close: values[values.length - 1], high: range.max, low: range.min, volume: values.reduce((sum, value) => sum + value, 0),
      }
    })
  }, [records, range])
  const max = Math.max(numericRange(candles.map((candle) => candle.high))?.max ?? 0, 1)
  const min = Math.min(numericRange(candles.map((candle) => candle.low))?.min ?? 0, 0)
  const spread = Math.max(max - min, 1)
  return <section className="panel candle-panel"><div className="panel-heading"><div><h2>{t('用量 K 线')}</h2></div><span className="panel-count">{t('{0} 个区间', candles.length)} · OHLC</span></div>{candles.length ? <div className="candle-chart" role="img" aria-label={t('Token 用量 K 线图')}>{candles.map((candle, index) => { const y = (value: number) => 150 - ((value - min) / spread) * 125; const bullish = candle.close >= candle.open; const bodyTop = Math.min(y(candle.open), y(candle.close)); const bodyHeight = Math.max(4, Math.abs(y(candle.close) - y(candle.open))); return <div className="candle-column" key={`${candle.label}-${index}`}><span className="candle-value">{formatTokens(candle.volume)}</span><div className="candle-plot"><i className="candle-wick" style={{ top: y(candle.high), height: Math.max(2, y(candle.low) - y(candle.high)) }} /><i className={`candle-body ${bullish ? 'bullish' : 'bearish'}`} style={{ top: bodyTop, height: bodyHeight }} /></div><span className="candle-label">{candle.label}</span></div>})}</div> : <div className="empty-state candle-empty"><ChartCandlestick size={22} />{t('当前筛选没有足够数据绘制 K 线')}</div>}<div className="candle-legend"><span><i className="legend-dot bullish" />{t('上升')}</span><span><i className="legend-dot bearish" />{t('下降')}</span><span>{t('每根 K 线顶部为区间总量')}</span></div></section>
}

function MetricCard({ label, value, exactValue, tone, icon }: { label: string; value: string; exactValue?: string; tone: string; icon?: ReactNode }) {
  return <div className={`metric-card tone-${tone}`}><div className="metric-top"><span>{label}</span>{icon && <span className="metric-icon">{icon}</span>}</div><strong key={value} title={exactValue} className="token-value-transition" style={{ '--metric-chars': value.length } as CSSProperties}>{value}</strong><div className="metric-rule" /></div>
}

function BoardSourcesToolbar() {
  const [open, setOpen] = useState(false)
  const [settings, setSettings] = useState<BoardWidgetSettings | null>(null)
  const [error, setError] = useState('')
  const current = useRef<BoardWidgetSettings | null>(null)
  const saveQueue = useRef(Promise.resolve())
  const drag = useRef<{ id: BoardWidgetId; pointerId: number } | null>(null)
  const drop = useRef<{ id: BoardWidgetId; after: boolean } | null>(null)
  const [indicator, setIndicator] = useState<{ id: BoardWidgetId; after: boolean } | null>(null)

  useEffect(() => {
    if (!open) return
    let active = true
    void saveQueue.current.then(loadBoardSettings).then((next) => {
      if (active) {
        current.current = next
        setSettings(next)
      }
    }).catch(() => { if (active) setError(t('读取看板来源失败，请关闭后重试。')) })
    return () => { active = false }
  }, [open])

  function commit(update: Pick<BoardWidgetSettings, 'visible' | 'order'>) {
    if (!current.current) return
    const next = { ...current.current, ...update }
    current.current = next
    setSettings(next)
    setError('')
    saveQueue.current = saveQueue.current.then(async () => {
      try {
        const saved = await saveBoardSettings(update)
        if (current.current !== next) return
        current.current = saved
        setSettings(saved)
      } catch {
        if (current.current !== next) return
        setError(t('保存失败，请重试。'))
        try {
          const saved = await loadBoardSettings()
          if (current.current === next) {
            current.current = saved
            setSettings(saved)
          }
        } catch { /* keep the changes visible until the service is available */ }
      }
    })
  }

  function toggle(id: BoardWidgetId) {
    const saved = current.current
    if (!saved) return
    const visible = saved.visible.includes(id) ? saved.visible.filter((item) => item !== id) : [...saved.visible, id]
    commit({ visible, order: saved.order ?? boardWidgetOrder })
  }

  function reorder(id: BoardWidgetId, target: BoardWidgetId, after: boolean) {
    const saved = current.current
    if (!saved || id === target) return
    const original = saved.order ?? boardWidgetOrder
    const order = original.filter((item) => item !== id)
    order.splice(order.indexOf(target) + Number(after), 0, id)
    if (order.some((item, index) => item !== original[index])) commit({ visible: saved.visible, order })
  }

  function cancelDrag() {
    drag.current = null
    drop.current = null
    setIndicator(null)
  }

  function close() {
    cancelDrag()
    setOpen(false)
  }

  const container = useBoardPopoverDismiss(open, close)
  const order = settings?.order ?? boardWidgetOrder
  return <div ref={container} className="board-source-toolbar" onKeyDown={(event) => {
    if (event.key === 'Escape' && open) close()
  }}>
    <button type="button" className="button button-quiet" aria-expanded={open} aria-controls={open ? 'board-source-panel' : undefined} onClick={() => {
      if (open) close()
      else { setSettings(null); setError(''); setOpen(true) }
    }}><Layers3 size={15} /><span>{t('看板来源')}</span><ChevronDown size={12} /></button>
    {open && <section className="board-source-panel" id="board-source-panel" aria-label={t('看板来源')}>
      {settings && <ol className="board-source-list" aria-label={t('来源顺序')}>{order.map((id) => {
        const option = boardWidgetOptions[boardWidgetOrder.indexOf(id)]
        const checked = settings.visible.includes(id)
        return <li key={id} data-board-source={id} className={indicator?.id === id ? (indicator.after ? 'drop-after' : 'drop-before') : undefined}>
          <label><input type="checkbox" checked={checked} onChange={() => toggle(id)} /><span>{t(option.label)}</span></label>
          <button type="button" className="board-source-grip" aria-label={t('调整顺序：{0}', t(option.label))} title={t('拖动排序；也可使用方向键')} onKeyDown={(event) => {
            if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return
            event.preventDefault()
            const after = event.key === 'ArrowDown'
            const target = order[order.indexOf(id) + (after ? 1 : -1)]
            if (target) reorder(id, target, after)
          }} onPointerDown={(event) => {
            if (event.button !== 0) return
            event.currentTarget.setPointerCapture(event.pointerId)
            drag.current = { id, pointerId: event.pointerId }
          }} onPointerMove={(event) => {
            if (drag.current?.pointerId !== event.pointerId) return
            const row = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>('[data-board-source]')
            const target = row?.dataset.boardSource as BoardWidgetId | undefined
            if (!row || !target || target === id || !event.currentTarget.closest('ol')?.contains(row)) {
              drop.current = null
              setIndicator(null)
              return
            }
            const bounds = row.getBoundingClientRect()
            const next = { id: target, after: event.clientY >= bounds.top + bounds.height / 2 }
            if (drop.current?.id === next.id && drop.current.after === next.after) return
            drop.current = next
            setIndicator(next)
          }} onPointerUp={(event) => {
            if (drag.current?.pointerId !== event.pointerId) return
            if (drop.current) reorder(id, drop.current.id, drop.current.after)
            cancelDrag()
          }} onPointerCancel={cancelDrag} onLostPointerCapture={cancelDrag}><GripVertical size={16} /></button>
        </li>
      })}</ol>}
      {settings && <p>{t('上方来源在最上层；隐藏不会改变卡片位置。')}</p>}
      {!settings && !error && <p role="status">{t('正在读取看板来源…')}</p>}
      {error && <p role="alert">{error}</p>}
    </section>}
  </div>
}




type ResizeDirection = 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw'
type BoardInteraction =
  | { kind: 'move'; indices: number[]; pointerId: number; startX: number; startY: number; origins: BoardCardGeometry[] }
  | { kind: 'resize'; indices: number[]; pointerId: number; startX: number; startY: number; origins: BoardCardGeometry[]; direction: ResizeDirection }
const BOARD_CARD_MIN_WIDTH = 210
const BOARD_CARD_MIN_HEIGHT = 64
const BOARD_SNAP_DISTANCE = 5
const BOARD_RESIZE_DIRECTIONS: ResizeDirection[] = ['n', 's', 'e', 'w', 'ne', 'nw', 'se', 'sw']

function cardsFromSharedLayout(layout: SharedBoardLayout): BoardCardGeometry[] {
  const viewportWidth = window.innerWidth > 0 ? window.innerWidth : layout.viewport.width
  const viewportHeight = window.innerHeight > 0 ? window.innerHeight : layout.viewport.height
  const scaleX = viewportWidth / Math.max(layout.viewport.width, 1)
  const scaleY = viewportHeight / Math.max(layout.viewport.height, 1)
  if (layout.revision === 0) return defaultBoardCards(viewportWidth, viewportHeight)
  const sizeScale = scaleX
  const fallback = defaultBoardCards(layout.viewport.width, layout.viewport.height)
  return Array.from({ length: 7 }, (_, index) => layout.cards[index] ?? fallback[index]).map((card) => {
    const width = Math.max(BOARD_CARD_MIN_WIDTH, Math.min(viewportWidth, card.width * sizeScale))
    const height = Math.max(BOARD_CARD_MIN_HEIGHT, Math.min(viewportHeight, card.height * sizeScale))
    return {
      width,
      height,
      x: Math.max(0, Math.min(viewportWidth - width, card.x * scaleX)),
      y: Math.max(0, Math.min(viewportHeight - height, card.y * scaleY)),
    }
  })
}

function geometryForResize(origin: BoardCardGeometry, direction: ResizeDirection, dx: number, dy: number): BoardCardGeometry {
  const nextCard = { ...origin }
  if (direction.includes('e')) nextCard.width = Math.max(BOARD_CARD_MIN_WIDTH, Math.min(window.innerWidth - nextCard.x, origin.width + dx))
  if (direction.includes('s')) nextCard.height = Math.max(BOARD_CARD_MIN_HEIGHT, Math.min(window.innerHeight - nextCard.y, origin.height + dy))
  if (direction.includes('w')) {
    nextCard.x = Math.max(0, Math.min(origin.x + origin.width - BOARD_CARD_MIN_WIDTH, origin.x + dx))
    nextCard.width = origin.width + origin.x - nextCard.x
  }
  if (direction.includes('n')) {
    nextCard.y = Math.max(0, Math.min(origin.y + origin.height - BOARD_CARD_MIN_HEIGHT, origin.y + dy))
    nextCard.height = origin.height + origin.y - nextCard.y
  }
  return nextCard
}


type BoardSnapBounds = { min: number; max: number } | 'leading' | 'trailing'

// For resizing, only the active edge moves; compare it to both stationary edges.
function snapBoardAxis(axis: 'x' | 'y', origins: BoardCardGeometry[], targets: BoardCardGeometry[], delta: number, bounds: BoardSnapBounds) {
  const size = axis === 'x' ? 'width' : 'height'
  const minSize = axis === 'x' ? BOARD_CARD_MIN_WIDTH : BOARD_CARD_MIN_HEIGHT
  const viewport = axis === 'x' ? window.innerWidth : window.innerHeight
  const trailing = bounds === 'trailing'
  const edges = targets.flatMap((target) => {
    const start = target[axis]
    if (typeof bounds !== 'string') return [start]
    const end = start + target[size]
    return trailing ? [end, start] : [start, end]
  })

  let match: { delta: number; guide: number; distance: number } | undefined
  for (const origin of origins) {
    const start = origin[axis]
    const end = start + origin[size]
    const anchor = trailing ? end : start
    let min: number
    let max: number
    if (typeof bounds === 'string') {
      min = (trailing ? start + minSize : 0) - anchor
      max = (trailing ? viewport : end - minSize) - anchor
    } else {
      ({ min, max } = bounds)
    }
    for (const edge of edges) {
      const alignedDelta = edge - anchor
      const distance = Math.abs(alignedDelta - delta)
      if (alignedDelta >= min && alignedDelta <= max && distance <= BOARD_SNAP_DISTANCE && (!match || distance < match.distance)) {
        match = { delta: alignedDelta, guide: edge, distance }
      }
    }
  }
  return { delta: match?.delta ?? delta, guide: match?.guide ?? null }
}

function BoardDashboard({ totals, editable }: { totals: number[]; editable: boolean }) {
  const periods = [
    [t('1小时'), 3600], [t('1天'), 86400], [t('7天'), 7 * 86400], [t('30天'), 30 * 86400], [t('90天'), 90 * 86400], [t('1年'), 365 * 86400],
  ] as const
  const [cards, setCards] = useState<BoardCardGeometry[]>(() => defaultBoardCards(window.innerWidth, window.innerHeight))
  const [snapGuides, setSnapGuides] = useState<{ x: number | null; y: number | null } | null>(null)
  const [danmaku, setDanmaku] = useState<DanmakuState>({ room_id: 0, real_room_id: 0, status: 'not-configured', authenticated: false, font_size: 16, error: null, revision: 0, messages: [] })
  const [boardSettings, setBoardSettings] = useState<BoardWidgetSettings>({ visible: boardWidgetOrder, order: boardWidgetOrder, revision: 0 })
  const tokenStyle = useTokenBoardStyle()
  const tokenCss = useMemo(() => compileTokenBoardCss(tokenStyle), [tokenStyle])
  const [selected, setSelected] = useState<Set<number>>(() => new Set())
  const cardsRef = useRef(cards)
  const selectedRef = useRef(selected)
  const interactionRef = useRef<BoardInteraction | null>(null)
  const layoutRevisionRef = useRef(0)
  const savingLayoutRef = useRef(false)
  useEffect(() => { cardsRef.current = cards }, [cards])
  useEffect(() => { selectedRef.current = selected }, [selected])
  useEffect(() => {
    let active = true
    const sync = async () => {
      try {
        const next = await loadDanmakuState()
        if (!active) return
        setDanmaku((current) => current.revision === next.revision && current.status === next.status && current.font_size === next.font_size && current.authenticated === next.authenticated && current.room_id === next.room_id ? current : next)
      } catch { /* preserve the most recent messages during a transient local request failure */ }
    }
    void sync()
    const timer = window.setInterval(() => void sync(), 1000)
    return () => { active = false; window.clearInterval(timer) }
  }, [])
  useEffect(() => {
    let active = true
    const sync = async () => {
      try {
        const next = await loadBoardSettings()
        if (!active) return
        setBoardSettings((current) => current.revision === next.revision ? current : next)
      } catch { /* keep the last shared module selection */ }
    }
    void sync()
    const timer = window.setInterval(() => void sync(), 1000)
    return () => { active = false; window.clearInterval(timer) }
  }, [])
  const persistCards = useCallback(async () => {
    savingLayoutRef.current = true
    try {
      const saved = await saveBoardLayout({ width: window.innerWidth, height: window.innerHeight }, cardsRef.current)
      layoutRevisionRef.current = saved.revision
    } catch {
      // Keep the local interaction responsive; the next successful edit will retry persistence.
    } finally {
      savingLayoutRef.current = false
    }
  }, [])
  useEffect(() => {
    let active = true
    const sync = async () => {
      if (interactionRef.current || savingLayoutRef.current) return
      try {
        const layout = await loadBoardLayout()
        if (!active || layout.revision <= layoutRevisionRef.current) return
        layoutRevisionRef.current = layout.revision
        const next = cardsFromSharedLayout(layout)
        cardsRef.current = next
        setCards(next)
      } catch { /* keep the last known shared layout */ }
    }
    void sync()
    const timer = window.setInterval(() => void sync(), 1000)
    return () => { active = false; window.clearInterval(timer) }
  }, [])
  useEffect(() => {
    let resizeTimer = 0
    const applyStableViewport = async () => {
      if (window.innerWidth < 1 || window.innerHeight < 1 || interactionRef.current) return
      try {
        const layout = await loadBoardLayout()
        const next = cardsFromSharedLayout(layout)
        cardsRef.current = next
        setCards(next)
      } catch { /* the normal layout poll will retry */ }
    }
    const schedule = () => {
      window.clearTimeout(resizeTimer)
      resizeTimer = window.setTimeout(() => void applyStableViewport(), 180)
    }
    schedule()
    window.addEventListener('resize', schedule)
    return () => { window.clearTimeout(resizeTimer); window.removeEventListener('resize', schedule) }
  }, [])
  useEffect(() => {
    if (!editable) return
    const move = (event: PointerEvent) => {
      const interaction = interactionRef.current
      if (!interaction || interaction.pointerId !== event.pointerId) return
      const current = cardsRef.current
      let moveX = event.clientX - interaction.startX
      let moveY = event.clientY - interaction.startY
      let xBounds: BoardSnapBounds | undefined
      let yBounds: BoardSnapBounds | undefined
      if (interaction.kind === 'move') {
        const minX = -Math.min(...interaction.origins.map((card) => card.x))
        const maxX = window.innerWidth - Math.max(...interaction.origins.map((card) => card.x + card.width))
        const minY = -Math.min(...interaction.origins.map((card) => card.y))
        const maxY = window.innerHeight - Math.max(...interaction.origins.map((card) => card.y + card.height))
        moveX = Math.max(minX, Math.min(maxX, moveX))
        moveY = Math.max(minY, Math.min(maxY, moveY))
        xBounds = { min: minX, max: maxX }
        yBounds = { min: minY, max: maxY }
      } else {
        const { direction } = interaction
        xBounds = direction.includes('e') ? 'trailing' : direction.includes('w') ? 'leading' : undefined
        yBounds = direction.includes('s') ? 'trailing' : direction.includes('n') ? 'leading' : undefined
      }

      if (window.frameElement?.getAttribute('data-board-snap') === 'on') {
        const targets = current.filter((_, index) => !interaction.indices.includes(index) && boardSettings.visible.includes(boardWidgetOptions[index].id))
        const x = xBounds ? snapBoardAxis('x', interaction.origins, targets, moveX, xBounds) : { delta: moveX, guide: null }
        const y = yBounds ? snapBoardAxis('y', interaction.origins, targets, moveY, yBounds) : { delta: moveY, guide: null }
        moveX = x.delta
        moveY = y.delta
        setSnapGuides(x.guide !== null || y.guide !== null ? { x: x.guide, y: y.guide } : null)
      } else {
        setSnapGuides(null)
      }
      const next = current.map((card, index) => {
        const selectedIndex = interaction.indices.indexOf(index)
        if (selectedIndex < 0) return card
        const origin = interaction.origins[selectedIndex]
        if (interaction.kind === 'move') return { ...origin, x: origin.x + moveX, y: origin.y + moveY }
        return geometryForResize(origin, interaction.direction, moveX, moveY)
      })
      cardsRef.current = next
      setCards(next)
    }
    const finish = (event: PointerEvent) => {
      if (interactionRef.current?.pointerId !== event.pointerId) return
      interactionRef.current = null
      setSnapGuides(null)
      void persistCards()
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', finish)
    window.addEventListener('pointercancel', finish)
    return () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', finish)
      window.removeEventListener('pointercancel', finish)
    }
  }, [editable, persistCards, boardSettings.visible])
  const startDrag = (index: number, event: ReactPointerEvent<HTMLDivElement>) => {
    if ((event.target as HTMLElement).closest('.board-resize-handle')) return
    if (event.ctrlKey || event.metaKey) {
      const next = new Set(selectedRef.current)
      if (next.has(index)) next.delete(index)
      else next.add(index)
      selectedRef.current = next
      setSelected(next)
      return
    }
    const indices = selectedRef.current.has(index) && selectedRef.current.size > 1 ? [...selectedRef.current] : [index]
    if (indices.length === 1) {
      const next = new Set(indices)
      selectedRef.current = next
      setSelected(next)
    }
    event.currentTarget.setPointerCapture(event.pointerId)
    interactionRef.current = { kind: 'move', indices, pointerId: event.pointerId, origins: indices.map((item) => cardsRef.current[item]), startX: event.clientX, startY: event.clientY }
  }
  const startResize = (index: number, direction: ResizeDirection, event: ReactPointerEvent<HTMLSpanElement>) => {
    const indices = selectedRef.current.has(index) && selectedRef.current.size > 1 ? [...selectedRef.current] : [index]
    if (!selectedRef.current.has(index)) {
      const next = new Set([index])
      selectedRef.current = next
      setSelected(next)
    }
    event.currentTarget.setPointerCapture(event.pointerId)
    interactionRef.current = { kind: 'resize', indices, direction, pointerId: event.pointerId, origins: indices.map((item) => cardsRef.current[item]), startX: event.clientX, startY: event.clientY }
  }
  const cardContents: ReactNode[] = periods.map(([label], index) => {
    const total = totals[index] ?? 0
    return <MetricCard label={t('{0} Token 消耗', t(label))} value={formatExact(total)} tone={['lime', 'orange', 'teal', 'violet', 'lime', 'orange'][index]} />
  })
  cardContents.push(<DanmakuCard state={danmaku} />)
  return <section className={`board-dashboard board-free-canvas${editable ? '' : ' board-view-only'}`} data-token-theme={tokenStyle.preset} onPointerDown={editable ? (event) => { if (event.target === event.currentTarget) { const next = new Set<number>(); selectedRef.current = next; setSelected(next) } } : undefined}>
    {tokenCss && <style>{tokenCss}</style>}
    {snapGuides?.x != null && <span aria-hidden="true" className="board-snap-guide board-snap-guide-x" style={{ left: snapGuides.x }} />}
    {snapGuides?.y != null && <span aria-hidden="true" className="board-snap-guide board-snap-guide-y" style={{ top: snapGuides.y }} />}
    {cards.map((_card, index) => {
    const id = boardWidgetOptions[index].id
    if (!boardSettings.visible.includes(id)) return null
    const order = boardSettings.order ?? boardWidgetOrder
    return <div
      className={`board-draggable-card${index < periods.length ? ' board-token-card' : ''}${selected.has(index) ? ' selected' : ''}`}
      key={id}
      data-board-widget={id}
      style={{ '--board-card-layer': order.length - order.indexOf(id), width: cards[index].width, height: cards[index].height, transform: `translate3d(${cards[index].x}px, ${cards[index].y}px, 0)` } as CSSProperties}
      onPointerDown={editable ? (event) => startDrag(index, event) : undefined}
      title={editable ? t('拖动卡片移动，拖动边缘调整大小') : undefined}
    >{cardContents[index]}{editable && BOARD_RESIZE_DIRECTIONS.map((direction) => <span key={direction} className={`board-resize-handle resize-${direction}`} onPointerDown={(event) => startResize(index, direction, event)} />)}</div>
  })}</section>
}

function ModelRanking({ rows }: { rows: GroupRow[] }) {
  const max = Math.max(rows[0]?.total_tokens ?? 1, 1)
  return <section className="analysis-panel panel" aria-label={t('模型消耗排行')}>
    <div className="overview-visual-meta"><span className="panel-count">{t('{0} 个模型', rows.length)}</span></div>
    {rows.length ? <div className="ranking-list" role="region" aria-label={t('模型消耗排行列表')} tabIndex={0}>{rows.map((row, index) => <div className="ranking-row" key={row.label}><span className="rank">{String(index + 1).padStart(2, '0')}</span><div className="ranking-main"><div className="ranking-label"><strong>{row.label || t('未知模型')}</strong><span>{t('{0} 条记录', row.records.toLocaleString(locale()))}</span></div><div className="ranking-track"><div style={{ width: `${(row.total_tokens / max) * 100}%` }} /></div></div><strong className="ranking-total">{formatTokens(row.total_tokens)}</strong></div>)}</div> : <div className="empty-state">{t('暂无模型用量')}</div>}
  </section>
}

function SourceBadge({ source }: { source: SourceId }) { return <span className="source-badge"><i style={{ background: sourceMeta[source].accent }} />{t(sourceMeta[source].label)}</span> }

export default App
