import { locale, t } from './i18n'
import { useEffect, useRef } from 'react'
import { ChevronRight, RefreshCw, X } from 'lucide-react'
import type { BackupStatus, DashboardData, SourceSettings, SyncSettings } from './types'
import { useModalFocus } from './useModalFocus'
import Dropdown from './Dropdown'
import { sourceIds, sourceMeta } from './data'

export function syncTimeLabel(timestamp: number): string {
  if (!timestamp) return t('尚未同步')
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - timestamp)
  if (seconds < 10) return t('刚刚同步')
  if (seconds < 60) return t('{0} 秒前同步', seconds)
  if (seconds < 3600) return t('{0} 分钟前同步', Math.floor(seconds / 60))
  if (seconds < 86400) return t('{0} 小时前同步', Math.floor(seconds / 3600))
  return t('{0} 天前同步', Math.floor(seconds / 86400))
}

export default function SyncSettingsPanel({ sync, sources, statuses, errors, backup, loading, message, onUpdate, onSync, onRebuild, onPricing, onClose }: {
  sync: SyncSettings
  sources: SourceSettings
  statuses: DashboardData['source_status']
  errors: DashboardData['source_errors']
  backup?: BackupStatus
  loading: boolean
  message: string
  onUpdate: (value: SyncSettings) => void
  onSync: () => void
  onRebuild: () => void
  onPricing: () => void
  onClose: () => void
}) {
  const lastChecked = Math.max(0, ...Object.values(sources).map((item) => item.last_checked_at || 0))
  const warnings = sourceIds.filter((source) => errors[source] || statuses[source] === 'error' || statuses[source] === 'needs-permission')
  const drawerRef = useRef<HTMLElement>(null)
  useModalFocus(drawerRef)
  useEffect(() => {
    const handler = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose() }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [onClose])
  return <div className="settings-drawer-backdrop" onClick={onClose}>
    <aside ref={drawerRef} tabIndex={-1} className="settings-drawer" role="dialog" aria-modal="true" aria-label={t('用量设置')} onClick={(event) => event.stopPropagation()}>
      <div className="settings-drawer-head"><div><h2>{t('用量设置')}</h2></div><button className="drawer-close" aria-label={t('关闭用量设置')} onClick={onClose}><X size={19} /></button></div>
      <div className="settings-drawer-content">
        <span className="drawer-label">{t('数据来源')}</span>
        <div className="sync-settings-card">
          <div className="sync-setting-section">
            <div className="sync-setting-title"><div><strong>{t('自动扫描会话与用量')}</strong><p>{t('从 {0} 个本地来源导入用量', Object.keys(sources).length)}</p></div><label className="sync-switch"><input type="checkbox" aria-label={t('自动扫描会话与用量')} checked={sync.auto_scan_enabled} onChange={(event) => onUpdate({ ...sync, auto_scan_enabled: event.target.checked })} /><span /></label></div>
            <p className="sync-setting-description">{t('启动和打开总览时检查一次。总览打开时按统一间隔检查，后台每 10 分钟检查；关闭后仍可手动同步。')}{syncTimeLabel(lastChecked)}。</p>
            <div className="sync-setting-actions"><Dropdown label={t('扫描间隔')} value={String(sync.interval_seconds)} disabled={!sync.auto_scan_enabled}
              onChange={(value) => onUpdate({ ...sync, interval_seconds: Number(value) })}
              options={[{ value: '5', label: t('每 5 秒') }, { value: '10', label: t('每 10 秒') }, { value: '30', label: t('每 30 秒') }, { value: '60', label: t('每 1 分钟') }, { value: '300', label: t('每 5 分钟') }, { value: '600', label: t('每 10 分钟') }]} /><button className="button button-quiet" disabled={loading} onClick={onSync}><RefreshCw size={14} className={loading ? 'spin' : ''} />{loading ? t('同步中') : t('立即同步')}</button></div>
            {warnings.length > 0 && <details className="sync-setting-warning"><summary>{t('扫描提示（{0}）', warnings.length)}</summary><ul>{warnings.map((source) => <li key={source}><strong>{t(sourceMeta[source].label)}：</strong>{errors[source] ?? (statuses[source] === 'needs-permission' ? t('需要权限') : t('读取失败'))}</li>)}</ul></details>}
            {message && <p className="sync-setting-message" role="status">{message}</p>}
          </div>
          <div className="sync-setting-section"><div className="sync-setting-title"><div><strong>{t('Codex 用量维护')}</strong><p>{t('重新读取本机会话，并保留此前已采集但原日志已移除的记录。')}</p></div></div><button className="button button-quiet" disabled={loading} onClick={onRebuild}>{t('重建 Codex 用量')}</button></div>
          {backup && <div className="sync-setting-section"><div className="sync-setting-title"><div><strong>{t('本地用量备份')}</strong><p>{t('每 24 小时备份，保留最近 10 份；历史明细长期保留。')}</p></div></div><p className="sync-setting-description">{backup.last_backup_at ? t('最近备份：{0} · 共 {1} 份', new Date(backup.last_backup_at * 1000).toLocaleString(locale()), backup.count) : t('尚未生成备份')} · data/backups</p>{backup.error && <p className="sync-setting-warning" role="status">{backup.error}</p>}</div>}
        </div>
        <span className="drawer-label">{t('定价')}</span>
        <button className="pricing-open-card" onClick={onPricing}><div><strong>{t('模型定价')}</strong><span>{t('管理模型价格，选择并同步 models.dev 定价')}</span></div><ChevronRight size={18} /></button>
      </div>
    </aside>
  </div>
}
