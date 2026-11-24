import { locale, t } from './i18n'
import { useEffect, useMemo, useRef, useState } from 'react'
import { ChevronLeft, Download, Pencil, Plus, RefreshCw, Search, Trash2, X } from 'lucide-react'
import { deleteModelPrice, loadPricing, loadPricingCatalog, saveModelPrice, savePricingSettings, syncModelPrices } from './api'
import type { CatalogModel, ModelPrice, PricingState } from './types'
import { useModalFocus } from './useModalFocus'
import { selectCatalogResults } from './pricingSelection'

type Draft = { model_id: string; name: string; provider: string; input: string; output: string; cache_read: string; cache_write: string }
const emptyDraft = (): Draft => ({ model_id: '', name: '', provider: '', input: '', output: '', cache_read: '', cache_write: '' })
const showRate = (value: number | null) => value === null ? '—' : `$${value.toLocaleString('en-US', { maximumFractionDigits: 8 })}`
const messageFor = (error: unknown) => error instanceof Error ? error.message : t('操作失败，请稍后重试')
const PRICE_PAGE_SIZE = 100

export default function PricingDialog({ state, usedModels, onChange, onClose }: { state: PricingState; usedModels: string[]; onChange: (state: PricingState) => void; onClose: () => void }) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const [catalog, setCatalog] = useState<CatalogModel[] | null>(null)
  const [pickerOpen, setPickerOpen] = useState(false)
  const [enableAfterSelect, setEnableAfterSelect] = useState(false)
  const [selected, setSelected] = useState<Set<string>>(() => new Set(state.selected))
  const [query, setQuery] = useState('')
  const [priceQuery, setPriceQuery] = useState('')
  const [pricePage, setPricePage] = useState(1)
  const [editing, setEditing] = useState<Draft | null>(null)
  const [editingExisting, setEditingExisting] = useState(false)
  const dialogRef = useRef<HTMLElement>(null)
  const pickerRef = useRef<HTMLElement>(null)
  const editorRef = useRef<HTMLElement>(null)
  useModalFocus(dialogRef, !pickerOpen && !editing)
  useModalFocus(pickerRef, pickerOpen)
  useModalFocus(editorRef, Boolean(editing))
  const used = useMemo(() => new Set(usedModels.map((item) => item.toLowerCase().split('/').at(-1))), [usedModels])
  const catalogByKey = useMemo(() => new Map((catalog ?? []).map((item) => [item.key, item])), [catalog])
  const visibleCatalog = useMemo(() => {
    const term = query.trim().toLowerCase()
    const filtered = (catalog ?? []).filter((item) => !term || `${item.provider} ${item.model_id} ${item.name}`.toLowerCase().includes(term))
    return filtered.sort((a, b) => Number(selected.has(b.key)) - Number(selected.has(a.key)) || Number(used.has(b.model_id.toLowerCase())) - Number(used.has(a.model_id.toLowerCase())) || a.provider.localeCompare(b.provider) || a.model_id.localeCompare(b.model_id))
  }, [catalog, query, selected, used])
  const filteredPrices = useMemo(() => {
    const term = priceQuery.trim().toLowerCase()
    return term ? state.prices.filter((item) => `${item.name} ${item.model_id} ${item.provider}`.toLowerCase().includes(term)) : state.prices
  }, [state.prices, priceQuery])
  const pricePageCount = Math.max(1, Math.ceil(filteredPrices.length / PRICE_PAGE_SIZE))
  const currentPricePage = Math.min(pricePage, pricePageCount)
  const pricePageRows = filteredPrices.slice((currentPricePage - 1) * PRICE_PAGE_SIZE, currentPricePage * PRICE_PAGE_SIZE)

  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      if (editing) setEditing(null)
      else if (pickerOpen) setPickerOpen(false)
      else onClose()
    }
    document.addEventListener('keydown', handler)
    return () => document.removeEventListener('keydown', handler)
  }, [editing, pickerOpen, onClose])

  async function openPicker(enableAuto = false) {
    setError(''); setMessage(''); setEnableAfterSelect(enableAuto); setPickerOpen(true); setSelected(new Set(state.selected)); setQuery('')
    if (catalog) return
    setBusy(true)
    try { setCatalog((await loadPricingCatalog()).models) } catch (reason) { setError(messageFor(reason)) } finally { setBusy(false) }
  }

  async function runSync() {
    if (!state.selected.length) { await openPicker(); return }
    setBusy(true); setError(''); setMessage('')
    try {
      const result = await syncModelPrices()
      onChange(result.state)
      setMessage(t('同步了 {0} 个模型，保留 {1} 个手动价格。', result.updated, result.preserved))
    } catch (reason) {
      setError(messageFor(reason))
      try { onChange(await loadPricing()) } catch { /* Keep the last known settings. */ }
    } finally { setBusy(false) }
  }

  async function saveSelection() {
    setBusy(true); setError(''); setMessage('')
    try {
      const next = await savePricingSettings({ selected: [...selected], ...(enableAfterSelect && selected.size ? { auto_sync: true } : {}) })
      const saved = new Set(next.selected)
      if (saved.size !== selected.size || [...selected].some((key) => !saved.has(key))) {
        throw new Error(t('模型选择未完整保存，请重启 TokenUsage 服务后重试。'))
      }
      onChange(next); setPickerOpen(false)
      if (next.selected.length) {
        const result = await syncModelPrices()
        onChange(result.state)
        setMessage(t('同步了 {0} 个模型，保留 {1} 个手动价格。', result.updated, result.preserved))
      } else setMessage(t('已清空同步模型。'))
    } catch (reason) {
      setError(messageFor(reason))
      try { onChange(await loadPricing()) } catch { /* Keep the last known settings. */ }
    } finally { setBusy(false) }
  }

  async function setAutomatic(enabled: boolean) {
    if (enabled && !state.selected.length) { await openPicker(true); setMessage(t('选择模型并保存后将开启自动同步。')); return }
    setBusy(true); setError(''); setMessage('')
    try {
      const next = await savePricingSettings({ auto_sync: enabled })
      onChange(next)
      setMessage(enabled ? t('已开启自动定价同步，每 6 小时最多更新一次。') : t('已关闭自动定价同步。'))
    } catch (reason) { setError(messageFor(reason)) } finally { setBusy(false) }
  }

  function toggleModel(item: CatalogModel) {
    setSelected((previous) => {
      const next = new Set(previous)
      if (next.has(item.key)) next.delete(item.key)
      else {
        // The dashboard has one price per model ID. Switching provider replaces the previous choice.
        for (const key of next) if (catalogByKey.get(key)?.model_id.toLowerCase() === item.model_id.toLowerCase()) next.delete(key)
        next.add(item.key)
      }
      return next
    })
  }

  function editPrice(item?: ModelPrice) {
    setEditingExisting(Boolean(item))
    setEditing(item ? { model_id: item.model_id, name: item.name, provider: item.provider, input: String(item.input), output: String(item.output), cache_read: item.cache_read === null ? '' : String(item.cache_read), cache_write: item.cache_write === null ? '' : String(item.cache_write) } : emptyDraft())
    setError('')
  }

  async function savePrice() {
    if (!editing) return
    const required = (value: string) => value.trim() === '' ? Number.NaN : Number(value)
    const optional = (value: string) => value.trim() === '' ? null : Number(value)
    const value = { model_id: editing.model_id.trim(), name: editing.name.trim(), provider: editing.provider.trim(), input: required(editing.input), output: required(editing.output), cache_read: optional(editing.cache_read), cache_write: optional(editing.cache_write) }
    if (!value.model_id || [value.input, value.output, value.cache_read, value.cache_write].some((number) => number !== null && (!Number.isFinite(number) || number < 0))) { setError(t('请填写模型 ID，以及有效的非负价格。')); return }
    setBusy(true); setError('')
    try { onChange(await saveModelPrice(value)); setEditing(null); setMessage(t('模型定价已保存，后续自动同步会保留此手动价格。')) }
    catch (reason) { setError(messageFor(reason)) } finally { setBusy(false) }
  }

  async function removePrice(modelId: string) {
    setBusy(true); setError('')
    try { onChange(await deleteModelPrice(modelId)); setMessage(t('已移除该定价及对应的同步选择。')) }
    catch (reason) { setError(messageFor(reason)) } finally { setBusy(false) }
  }

  return <div className="pricing-backdrop" onClick={onClose}>
    <section ref={dialogRef} tabIndex={-1} className="pricing-dialog" role="dialog" aria-modal="true" aria-label={t('模型定价')} onClick={(event) => event.stopPropagation()}>
      <header className="pricing-head"><div><button className="drawer-close" aria-label={t('返回')} onClick={onClose}><ChevronLeft size={19} /></button><div><h2>{t('模型定价')}</h2></div></div><button className="drawer-close" aria-label={t('关闭定价')} onClick={onClose}><X size={19} /></button></header>
      <div className="pricing-content">
        <div className="pricing-sync-card"><div className="pricing-sync-copy"><RefreshCw size={19} /><div><strong>{t('models.dev 定价同步 ·') + ' '}{state.auto_sync ? t('已开启') : t('已关闭')}</strong><span>{state.last_synced_at ? t('上次同步 {0}', new Date(state.last_synced_at * 1000).toLocaleString(locale())) : t('尚未同步')} · {t('已选择 {0} 个模型', state.selected.length)}</span></div></div><div className="pricing-sync-actions"><label className="sync-switch"><input type="checkbox" aria-label={t('自动同步模型定价')} checked={state.auto_sync} disabled={busy} onChange={(event) => void setAutomatic(event.target.checked)} /><span /></label><button className="button button-quiet" disabled={busy} onClick={() => void openPicker()}>{t('选择模型')}</button><button className="button button-quiet" disabled={busy} onClick={() => void runSync()}>{busy ? t('处理中…') : t('立即同步')}</button></div></div>
        <p className="pricing-note">{t('价格以 USD / 每百万 Token 计，仅用于估算；实际供应商、套餐和历史价格可能不同。手动价格不会被同步覆盖。')}</p>
        {(error || state.last_error) && <p className="pricing-error" role="alert">{error || (state.last_error ? t('上次同步失败：{0}', state.last_error) : '')}</p>}
        {message && <p className="pricing-message" role="status">{message}</p>}
        <div className="pricing-table-head"><div><h3>{t('模型定价') + ' '}<span>· {state.prices.length}</span></h3><small>{t('USD / 每百万 Tokens')}</small></div><div><button className="button button-quiet" disabled={busy} onClick={() => void openPicker()}><Download size={14} />{t('从 models.dev 导入')}</button><button className="button button-primary" disabled={busy} onClick={() => editPrice()}><Plus size={15} />{t('新增定价')}</button></div></div>
        {state.prices.length > 0 && <label className="pricing-list-search"><Search size={15} /><input value={priceQuery} onChange={(event) => { setPriceQuery(event.target.value); setPricePage(1) }} placeholder={t('搜索已导入的模型或供应商')} /></label>}
        <div className="pricing-table-wrap"><table className="pricing-table"><thead><tr><th>{t('模型')}</th><th>{t('输入')}</th><th>{t('输出')}</th><th>{t('缓存命中')}</th><th>{t('缓存写入')}</th><th aria-label={t('操作')} /></tr></thead><tbody>{pricePageRows.map((item) => <tr key={item.model_id.toLowerCase()}><td><strong>{item.name}</strong><small>{item.model_id} · {item.provider || t('自定义')}{item.origin === 'manual' ? ' ' + t('· 手动') : ''}</small></td><td>{showRate(item.input)}</td><td>{showRate(item.output)}</td><td>{showRate(item.cache_read)}</td><td>{showRate(item.cache_write)}</td><td><div className="price-row-actions"><button aria-label={t('编辑 {0} 定价', item.model_id)} disabled={busy} onClick={() => editPrice(item)}><Pencil size={15} /></button><button aria-label={t('删除 {0} 定价', item.model_id)} disabled={busy} onClick={() => void removePrice(item.model_id)}><Trash2 size={15} /></button></div></td></tr>)}</tbody></table>{!filteredPrices.length && <div className="empty-state">{state.prices.length ? t('没有匹配的模型定价') : t('暂无模型定价，可从 models.dev 导入或手动新增')}</div>}</div>
        {filteredPrices.length > PRICE_PAGE_SIZE && <div className="pricing-pagination"><span>{t('{0} 个模型 · 第 {1}/{2} 页', filteredPrices.length.toLocaleString(locale()), currentPricePage, pricePageCount)}</span><div><button disabled={currentPricePage <= 1} onClick={() => setPricePage(currentPricePage - 1)}>{t('上一页')}</button><button disabled={currentPricePage >= pricePageCount} onClick={() => setPricePage(currentPricePage + 1)}>{t('下一页')}</button></div></div>}
      </div>
      {pickerOpen && <div className="pricing-submodal-backdrop">
        <section ref={pickerRef} tabIndex={-1} className="pricing-submodal catalog-dialog" role="dialog" aria-modal="true" aria-label={t('选择同步模型')}>
          <div className="pricing-submodal-head">
            <div><h3>{t('选择同步模型')}</h3><p>{t('同一模型只选一个供应商。全选保留已有选择，优先选择可识别的官方来源；其余按列表顺序选择。')}</p></div>
            <button className="drawer-close" aria-label={t('关闭模型选择')} onClick={() => setPickerOpen(false)}><X size={18} /></button>
          </div>
          {error && <p className="pricing-error" role="alert">{error}</p>}
          {message && <p className="pricing-message" role="status">{message}</p>}
          <label className="catalog-search"><Search size={16} /><input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={t('搜索模型或供应商')} /></label>
          <div className="catalog-toolbar">
            <span>{t('已选 {0} 个模型 · {1} 条结果', selected.size.toLocaleString(locale()), visibleCatalog.length.toLocaleString(locale()))}</span>
            <div className="catalog-toolbar-actions">
              <button disabled={busy || !catalog || !visibleCatalog.length} onClick={() => setSelected((current) => selectCatalogResults(current, visibleCatalog, catalogByKey))}>{t('全选当前结果')}</button>
              <button disabled={busy || !selected.size} onClick={() => setSelected(new Set())}>{t('清空选择')}</button>
            </div>
          </div>
          <div className="catalog-list">
            {busy && !catalog ? <div className="empty-state">{t('正在读取 models.dev…')}</div> : visibleCatalog.slice(0, 150).map((item) => <label className="catalog-row" key={item.key}><input type="checkbox" checked={selected.has(item.key)} onChange={() => toggleModel(item)} /><span><strong>{item.name}</strong><small>{item.provider} / {item.model_id}</small></span><span className="catalog-rate">{showRate(item.input)} / {showRate(item.output)}</span></label>)}
            {!busy && catalog && !visibleCatalog.length && <div className="empty-state">{t('没有匹配的模型')}</div>}
          </div>
          <div className="catalog-footer"><span>{t('最多显示前 150 条；全选会包含所有匹配结果')}</span><div><button className="button button-quiet" onClick={() => setPickerOpen(false)}>{t('取消')}</button><button className="button button-primary" disabled={busy || !catalog} onClick={() => void saveSelection()}>{busy ? t('同步中…') : t('保存并同步')}</button></div></div>
        </section>
      </div>}
      {editing && <div className="pricing-submodal-backdrop"><section ref={editorRef} tabIndex={-1} className="pricing-submodal price-editor" role="dialog" aria-modal="true" aria-label={editingExisting ? t('编辑模型定价') : t('新增模型定价')}><div className="pricing-submodal-head"><div><h3>{editingExisting ? t('编辑模型定价') : t('新增模型定价')}</h3><p>{t('单位：美元 / 每百万 Token')}</p></div><button className="drawer-close" aria-label={t('关闭定价编辑')} onClick={() => setEditing(null)}><X size={18} /></button></div><div className="price-form">{([['model_id', t('模型 ID')], ['name', t('显示名称')], ['provider', t('供应商')], ['input', t('输入价格')], ['output', t('输出价格')], ['cache_read', t('缓存命中价格（可留空）')], ['cache_write', t('缓存写入价格（可留空）')]] as const).map(([key, label]) => <label key={key}><span>{label}</span><input type={['input', 'output', 'cache_read', 'cache_write'].includes(key) ? 'number' : 'text'} min={0} step="any" disabled={key === 'model_id' && editingExisting} value={editing[key]} onChange={(event) => setEditing({ ...editing, [key]: event.target.value })} /></label>)}</div>{error && <p className="pricing-error" role="alert">{error}</p>}<div className="price-form-actions"><button className="button button-quiet" onClick={() => setEditing(null)}>{t('取消')}</button><button className="button button-primary" disabled={busy} onClick={() => void savePrice()}>{t('保存')}</button></div></section></div>}
    </section>
  </div>
}
