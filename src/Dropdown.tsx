import { Check, ChevronDown, Search, X } from 'lucide-react'
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent } from 'react'
import { createPortal } from 'react-dom'
import { locale, t } from './i18n'

export type DropdownOption = { value: string; label: string; lang?: string }
type DropdownProps = {
  label: string
  value: string
  options: readonly DropdownOption[]
  onChange: (value: string) => void
  variant?: 'pill' | 'field'
  disabled?: boolean
  searchPlaceholder?: string
  menuWidth?: number
}

const GAP = 7
const EDGE = 10

export default function Dropdown({
  label, value, options, onChange, variant = 'field', disabled = false,
  searchPlaceholder, menuWidth = searchPlaceholder ? 296 : 176,
}: DropdownProps) {
  const id = useId()
  const anchor = useRef<HTMLButtonElement>(null)
  const popup = useRef<HTMLDivElement>(null)
  const list = useRef<HTMLUListElement>(null)
  const search = useRef<HTMLInputElement>(null)
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const [cursor, setCursor] = useState(0)
  const [position, setPosition] = useState<CSSProperties | null>(null)
  const positioned = position !== null
  const labelForValue = options.find((option) => option.value === value)?.label ?? value
  const filtered = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase(locale())
    if (!needle) return options
    return options.filter((option) => option.label.toLocaleLowerCase(locale()).includes(needle)
      || option.value.toLocaleLowerCase(locale()).includes(needle))
  }, [options, query])
  const active = filtered.length ? Math.min(cursor, filtered.length - 1) : -1
  const activeId = active < 0 ? undefined : id + '-option-' + active

  const close = useCallback((restoreFocus = false) => {
    setOpen(false)
    if (restoreFocus) anchor.current?.focus({ preventScroll: true })
  }, [])
  function show() {
    if (disabled) return
    setQuery('')
    setCursor(Math.max(0, options.findIndex((option) => option.value === value)))
    setPosition(null)
    setOpen(true)
  }
  function select(option: DropdownOption) {
    close(true)
    if (option.value !== value) onChange(option.value)
  }

  useLayoutEffect(() => {
    if (!open) return
    const measure = () => {
      const rect = anchor.current?.getBoundingClientRect()
      if (!rect) return
      const viewport = window.visualViewport
      const leftEdge = viewport?.offsetLeft ?? 0
      const topEdge = viewport?.offsetTop ?? 0
      const rightEdge = leftEdge + (viewport?.width ?? document.documentElement.clientWidth)
      const bottomEdge = topEdge + (viewport?.height ?? window.innerHeight)
      if (rect.bottom < topEdge || rect.top > bottomEdge) { close(); return }
      const width = Math.min(Math.max(rect.width, menuWidth), rightEdge - leftEdge - EDGE * 2)
      const left = Math.max(leftEdge + EDGE, Math.min(rect.left, rightEdge - EDGE - width))
      const below = bottomEdge - rect.bottom - GAP - EDGE
      const above = rect.top - topEdge - GAP - EDGE
      const desired = Math.min(350, 14 + options.length * 34 + (searchPlaceholder ? 50 : 0))
      const upward = below < desired && above > below
      const maxHeight = Math.max(0, Math.min(350, upward ? above : below))
      setPosition(upward
        ? { left, width, maxHeight, bottom: window.innerHeight - rect.top + GAP }
        : { left, width, maxHeight, top: rect.bottom + GAP })
    }
    const onScroll = (event: Event) => {
      if (!(event.target instanceof Node) || !popup.current?.contains(event.target)) measure()
    }
    measure()
    window.addEventListener('resize', measure)
    window.addEventListener('scroll', onScroll, true)
    window.visualViewport?.addEventListener('resize', measure)
    window.visualViewport?.addEventListener('scroll', measure)
    return () => {
      window.removeEventListener('resize', measure)
      window.removeEventListener('scroll', onScroll, true)
      window.visualViewport?.removeEventListener('resize', measure)
      window.visualViewport?.removeEventListener('scroll', measure)
    }
  }, [open, menuWidth, options.length, searchPlaceholder, close])

  useLayoutEffect(() => {
    if (!open || !positioned) return
    const focus = search.current && window.matchMedia('(pointer: fine)').matches ? search.current : list.current
    focus?.focus({ preventScroll: true })
  }, [open, positioned])

  useEffect(() => {
    if (!open) return
    const check = (event: Event) => {
      if (!(event.target instanceof Node) || anchor.current?.contains(event.target) || popup.current?.contains(event.target)) return
      close()
    }
    document.addEventListener('pointerdown', check, true)
    document.addEventListener('focusin', check)
    return () => {
      document.removeEventListener('pointerdown', check, true)
      document.removeEventListener('focusin', check)
    }
  }, [open, close])
  useEffect(() => { if (disabled) close() }, [disabled, close])

  useLayoutEffect(() => {
    if (!open || !positioned || active < 0) return
    const viewport = list.current
    const row = viewport?.children.item(active) as HTMLElement | null | undefined
    if (!row || !viewport) return
    const top = row.offsetTop
    const bottom = top + row.offsetHeight
    if (top < viewport.scrollTop) viewport.scrollTop = top
    else if (bottom > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = bottom - viewport.clientHeight
  }, [open, positioned, active, filtered.length])

  function menuKeys(event: KeyboardEvent) {
    if (event.nativeEvent.isComposing) return
    const editing = event.target instanceof HTMLInputElement
    if (event.key === 'Escape') {
      event.preventDefault()
      event.stopPropagation()
      close(true)
    } else if (event.key === 'Tab') {
      close(true)
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      const delta = event.key === 'ArrowDown' ? 1 : -1
      setCursor(Math.max(0, Math.min(filtered.length - 1, active + delta)))
    } else if (!editing && (event.key === 'Home' || event.key === 'End')) {
      event.preventDefault()
      setCursor(event.key === 'Home' ? 0 : Math.max(0, filtered.length - 1))
    } else if (event.key === 'Enter' || (!editing && event.key === ' ')) {
      event.preventDefault()
      if (active >= 0) select(filtered[active])
    }
  }

  const host = open ? anchor.current?.closest('[aria-modal="true"]') ?? document.body : null
  return <div className={'dropdown dropdown--' + variant}>
    <button ref={anchor} type="button" className="dropdown-trigger" disabled={disabled}
      aria-label={label + ': ' + labelForValue} aria-haspopup="listbox" aria-expanded={open}
      aria-controls={open ? id : undefined} title={labelForValue}
      onClick={() => open ? close(true) : show()}
      onKeyDown={(event) => {
        if (!open && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) { event.preventDefault(); show() }
        if (open && event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(true) }
      }}>
      {variant === 'pill' && <span className="dropdown-caption">{label}</span>}
      <span className="dropdown-value">{labelForValue}</span>
      <ChevronDown className="dropdown-chevron" size={14} aria-hidden="true" />
    </button>
    {host && createPortal(<div ref={popup} className="dropdown-panel" style={position ?? { visibility: 'hidden' }} onKeyDown={menuKeys}>
      {searchPlaceholder && <div className="dropdown-search">
        <Search size={15} aria-hidden="true" />
        <input ref={search} role="combobox" aria-label={searchPlaceholder} aria-autocomplete="list" aria-expanded="true"
          aria-controls={id} aria-activedescendant={activeId} value={query} placeholder={searchPlaceholder}
          onChange={(event) => { setQuery(event.target.value); setCursor(0); if (list.current) list.current.scrollTop = 0 }} />
        {query && <button type="button" className="dropdown-clear" tabIndex={-1} aria-label={t('清除搜索')} onClick={() => { setQuery(''); setCursor(0); if (list.current) list.current.scrollTop = 0; search.current?.focus() }}><X size={14} aria-hidden="true" /></button>}
      </div>}
      <ul ref={list} id={id} className="dropdown-list" role="listbox" aria-label={label} tabIndex={searchPlaceholder ? -1 : 0} aria-activedescendant={activeId}>
        {filtered.map((option, index) => (
          <li id={id + '-option-' + index} key={option.value} role="option" aria-selected={option.value === value}
            lang={option.lang} className={index === active ? 'dropdown-option is-active' : 'dropdown-option'}
            title={option.label} onMouseDown={(event) => event.preventDefault()} onClick={() => select(option)}>
            <span>{option.label}</span>
            {option.value === value && <Check size={15} aria-hidden="true" />}
          </li>
        ))}
      </ul>
      {!filtered.length && <div className="dropdown-empty" role="status">{t('没有匹配的选项')}</div>}
    </div>, host)}
  </div>
}
