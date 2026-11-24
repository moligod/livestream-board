import { locale, t } from './i18n'
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from 'react'
import { CalendarDays, ChevronLeft, ChevronRight, X } from 'lucide-react'
import { calendarMonthDays, localDateTimeParts, parseLocalDateTime, type DateTimeParts } from './dateRange'

type RangeValue = { start: number | null; end: number | null; liveEnd: boolean }
type Draft = { start: DateTimeParts; end: DateTimeParts; liveEnd: boolean }
type Month = { year: number; month: number }

const presets = [
  { label: '近 1 小时', seconds: 3600 },
  { label: '近 24 小时', seconds: 86400 },
  { label: '近 7 天', seconds: 7 * 86400 },
  { label: '近 30 天', seconds: 30 * 86400 },
]
const weekdays = ['日', '一', '二', '三', '四', '五', '六']

function monthForDate(value: string): Month | null {
  const match = /^(\d{4})-(\d{2})-\d{2}$/.exec(value)
  if (!match) return null
  const year = Number(match[1])
  const month = Number(match[2]) - 1
  return month >= 0 && month < 12 ? { year, month } : null
}

function initialDraft(value: RangeValue): Draft {
  const now = Math.floor(Date.now() / 1000)
  return {
    start: localDateTimeParts(value.start ?? now - 86400),
    end: localDateTimeParts(value.liveEnd ? now : value.end ?? now),
    liveEnd: value.liveEnd,
  }
}

export default function CustomRangePopover({ value, anchorRef, onApply, onClose }: {
  value: RangeValue
  anchorRef: RefObject<HTMLDivElement | null>
  onApply: (value: { start: number; end: number; liveEnd: boolean }) => void
  onClose: (restoreFocus?: boolean) => void
}) {
  const [draft, setDraft] = useState(() => initialDraft(value))
  const [activeField, setActiveField] = useState<'start' | 'end'>('start')
  const [month, setMonth] = useState<Month>(() => monthForDate(initialDraft(value).start.date)!)
  const [submitted, setSubmitted] = useState(false)
  const [position, setPosition] = useState<{ top: number; left: number } | null>(null)
  const popoverRef = useRef<HTMLDivElement>(null)
  const startDateRef = useRef<HTMLInputElement>(null)
  const days = useMemo(() => calendarMonthDays(month.year, month.month), [month])
  const start = parseLocalDateTime(draft.start.date, draft.start.time)
  const end = draft.liveEnd ? Math.floor(Date.now() / 1000) : parseLocalDateTime(draft.end.date, draft.end.time)
  const error = start === null || end === null ? t('请输入有效的开始和结束时间。') : start > end ? t('结束时间不能早于开始时间。') : ''

  useLayoutEffect(() => {
    const updatePosition = () => {
      if (window.innerWidth <= 760) { setPosition(null); return }
      const anchor = anchorRef.current?.getBoundingClientRect()
      const popover = popoverRef.current?.getBoundingClientRect()
      if (!anchor || !popover) return
      const top = Math.max(12, Math.min(anchor.bottom + 10, window.innerHeight - popover.height - 12))
      const left = Math.max(12, Math.min(anchor.left, window.innerWidth - popover.width - 12))
      setPosition((current) => current?.top === top && current.left === left ? current : { top, left })
    }
    updatePosition()
    window.addEventListener('resize', updatePosition)
    window.addEventListener('scroll', updatePosition, true)
    return () => { window.removeEventListener('resize', updatePosition); window.removeEventListener('scroll', updatePosition, true) }
  }, [anchorRef, submitted])
  useEffect(() => { startDateRef.current?.focus() }, [])
  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      if (event.target instanceof Node && !anchorRef.current?.contains(event.target)) onClose()
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); onClose(true) }
    }
    document.addEventListener('pointerdown', onPointerDown)
    document.addEventListener('keydown', onKeyDown)
    return () => { document.removeEventListener('pointerdown', onPointerDown); document.removeEventListener('keydown', onKeyDown) }
  }, [anchorRef, onClose])

  function activate(field: 'start' | 'end') {
    if (field === 'end' && draft.liveEnd) return
    setActiveField(field)
    const nextMonth = monthForDate(draft[field].date)
    if (nextMonth) setMonth(nextMonth)
  }

  function changeField(field: 'start' | 'end', part: keyof DateTimeParts, next: string) {
    setDraft((current) => ({ ...current, [field]: { ...current[field], [part]: next } }))
    if (part === 'date') {
      const nextMonth = monthForDate(next)
      if (nextMonth) setMonth(nextMonth)
    }
  }

  function selectDay(date: string) {
    setDraft((current) => ({ ...current, [activeField]: { ...current[activeField], date } }))
    const nextMonth = monthForDate(date)
    if (nextMonth) setMonth(nextMonth)
    if (activeField === 'start' && !draft.liveEnd) setActiveField('end')
  }

  function moveMonth(offset: number) {
    const next = new Date(month.year, month.month + offset, 1, 12)
    setMonth({ year: next.getFullYear(), month: next.getMonth() })
  }

  function setPreset(seconds: number) {
    const now = Math.floor(Date.now() / 1000)
    const next = { start: localDateTimeParts(now - seconds), end: localDateTimeParts(now), liveEnd: false }
    setDraft(next)
    setActiveField('start')
    setMonth(monthForDate(next.start.date)!)
    setSubmitted(false)
  }

  function apply() {
    setSubmitted(true)
    // Re-evaluate the moving end at confirmation instead of using a stale render-time value.
    const appliedEnd = draft.liveEnd ? Math.floor(Date.now() / 1000) : parseLocalDateTime(draft.end.date, draft.end.time)
    if (start === null || appliedEnd === null || start > appliedEnd) return
    onApply({ start, end: appliedEnd, liveEnd: draft.liveEnd })
  }

  return <div ref={popoverRef} className="custom-range-popover" id="custom-range-popover" role="dialog" aria-label={t('自定义时间范围')} aria-describedby="custom-range-help" style={position ?? undefined}>
    <div className="custom-range-head">
      <div><h2>{t('选择时间范围')}</h2></div>
      <button className="custom-range-close" onClick={() => onClose(true)} aria-label={t('关闭自定义时间')}><X size={17} /></button>
    </div>
    <div className="custom-range-presets" aria-label={t('快捷时间范围')}>
      {presets.map((preset) => <button key={preset.seconds} onClick={() => setPreset(preset.seconds)}>{t(preset.label)}</button>)}
    </div>
    <div className="custom-range-body">
      <div className="custom-range-fields">
        <div className={`custom-range-field ${activeField === 'start' ? 'active' : ''}`} onClick={() => activate('start')} onFocus={() => activate('start')}>
          <span>{t('开始时间')}</span>
          <div className="custom-range-inputs"><input ref={startDateRef} type="date" aria-label={t('开始日期')} value={draft.start.date} onChange={(event) => changeField('start', 'date', event.target.value)} /><input type="time" aria-label={t('开始时刻')} value={draft.start.time} onChange={(event) => changeField('start', 'time', event.target.value)} /></div>
        </div>
        <div className={`custom-range-field ${activeField === 'end' ? 'active' : ''} ${draft.liveEnd ? 'disabled' : ''}`} onClick={() => activate('end')} onFocus={() => activate('end')}>
          <span>{t('结束时间')}</span>
          <div className="custom-range-inputs"><input type="date" aria-label={t('结束日期')} value={draft.end.date} disabled={draft.liveEnd} onChange={(event) => changeField('end', 'date', event.target.value)} /><input type="time" aria-label={t('结束时刻')} value={draft.end.time} disabled={draft.liveEnd} onChange={(event) => changeField('end', 'time', event.target.value)} /></div>
        </div>
        <label className="custom-range-live"><input type="checkbox" checked={draft.liveEnd} onChange={(event) => {
          const liveEnd = event.target.checked
          setDraft((current) => ({ ...current, liveEnd, ...(liveEnd ? { end: localDateTimeParts(Math.floor(Date.now() / 1000)) } : {}) }))
          if (liveEnd) setActiveField('start')
        }} />{t('结束时间跟随当前时刻')}</label>
        <p id="custom-range-help">{t('所选时间会同步应用于总览、趋势和明细记录。')}</p>
        {submitted && error && <p className="custom-range-error" role="alert">{error}</p>}
        <div className="custom-range-actions"><button onClick={() => onClose(true)}>{t('取消')}</button><button className="confirm" onClick={apply}>{t('确定')}</button></div>
      </div>
      <div className="custom-range-calendar">
        <div className="custom-calendar-head"><button onClick={() => moveMonth(-1)} aria-label={t('上个月')}><ChevronLeft size={17} /></button><strong>{new Date(month.year, month.month, 1).toLocaleDateString(locale(), { year: 'numeric', month: 'long' })}</strong><button onClick={() => moveMonth(1)} aria-label={t('下个月')}><ChevronRight size={17} /></button></div>
        <div className="custom-calendar-weekdays">{weekdays.map((day) => <span key={day}>{t(day)}</span>)}</div>
        <div className="custom-calendar-days">{days.map((day) => {
          const selected = day.date === draft.start.date || day.date === draft.end.date
          const inRange = draft.start.date < day.date && day.date < draft.end.date
          const today = day.date === localDateTimeParts(Math.floor(Date.now() / 1000)).date
          return <button key={day.date} className={`${day.inMonth ? '' : 'outside'} ${inRange ? 'in-range' : ''} ${selected ? 'selected' : ''} ${today ? 'today' : ''}`} onClick={() => selectDay(day.date)} aria-label={t(activeField === 'start' ? '选择开始日期：{0}' : '选择结束日期：{0}', day.date)} aria-pressed={selected}>{day.day}</button>
        })}</div>
        <div className="custom-calendar-hint"><CalendarDays size={13} />{t(activeField === 'start' ? '选择开始日期' : '选择结束日期')}</div>
      </div>
    </div>
  </div>
}
