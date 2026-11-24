import { t } from './i18n'
import { useBoardPopoverDismiss } from './useBoardPopoverDismiss'
import { useCallback, useEffect, useRef, useState } from 'react'
import { Palette } from 'lucide-react'

const presets = [
  { id: 'classic', label: '经典深色', css: [
    '.token-card {',
    '  background: rgba(18, 26, 33, .96);',
    '  border-color: rgba(54, 72, 83, .88);',
    '  border-radius: 10px;',
    '  box-shadow: 0 14px 38px rgba(0, 0, 0, .22);',
    '}',
    '.token-label { color: #c4d0d6; }',
    '.token-value { color: #edf1f5; }',
    '.token-accent { height: 2px; }',
  ].join('\n') },
  { id: 'minimal', label: '透明极简', css: [
    '.token-card {',
    '  background: transparent;',
    '  border-color: transparent;',
    '  box-shadow: none;',
    '}',
    '.token-label { text-shadow: 0 1px 6px #000; }',
    '.token-value { text-shadow: 0 1px 6px #000; }',
    '.token-accent { opacity: 0; }',
  ].join('\n') },
  { id: 'neon', label: '霓虹青', css: [
    '.token-card {',
    '  background: linear-gradient(145deg, #062433, #10162d);',
    '  border-color: #278a9c;',
    '  box-shadow: inset 0 0 20px #26c5dd22, 0 0 12px #26c5dd32;',
    '}',
    '.token-label { color: #8cdeef; }',
    '.token-value { color: #b7f5ff; text-shadow: 0 0 10px #55def180; }',
    '.token-accent { background: #43def0; }',
  ].join('\n') },
  { id: 'light', label: '浅色清爽', css: [
    '.token-card {',
    '  background: #f2f7ff;',
    '  border-color: #a9bede;',
    '  box-shadow: 0 6px 18px #152d5140;',
    '}',
    '.token-label { color: #3c5275; }',
    '.token-value { color: #152842; }',
  ].join('\n') },
] as const

export type TokenBoardStyle = { preset: typeof presets[number]['id']; css: string }
export const defaultTokenBoardStyle: TokenBoardStyle = { preset: 'classic', css: '' }

const textProperties = new Set(['color', 'font-family', 'font-size', 'font-weight', 'letter-spacing', 'line-height', 'margin-top', 'opacity', 'text-align', 'text-shadow', 'visibility'])
const targets = new Map([
  ['.token-card', { selector: '.metric-card', props: new Set(['background', 'background-color', 'border-color', 'border-radius', 'border-width', 'box-shadow', 'opacity', 'padding']) }],
  ['.token-label', { selector: '.metric-top', props: textProperties }],
  ['.token-value', { selector: '.metric-card strong', props: textProperties }],
  ['.token-accent', { selector: '.metric-card .metric-rule', props: new Set(['background', 'background-color', 'height', 'opacity', 'visibility']) }],
])
const cssFunctions = new Set(['rgb', 'rgba', 'hsl', 'hsla', 'hwb', 'oklab', 'oklch', 'lab', 'lch', 'linear-gradient', 'radial-gradient', 'calc', 'clamp', 'min', 'max'])

export function compileTokenCss(source: string): string {
  const text = source.replace(/\/\*[\s\S]*?\*\//g, '').trim()
  const rules = /\s*(\.[a-z-]+)\s*\{([^{}]*)\}\s*/gy
  const result: string[] = []
  let offset = 0
  while (offset < text.length) {
    const match = rules.exec(text)
    const target = match && targets.get(match[1])
    if (!match || !target) throw new Error(t('不支持的选择器，请使用 .token-card、.token-label、.token-value 或 .token-accent。'))
    const declarations = match[2].split(';').map((item) => item.trim()).filter(Boolean).map((item) => {
      const separator = item.indexOf(':')
      const property = item.slice(0, separator).trim().toLowerCase()
      const value = item.slice(separator + 1).trim()
      if (separator < 1 || !target.props.has(property)
        || !/^[\p{L}\p{N}\s#.,()%+\-'"\/]+$/u.test(value)
        || [...value.matchAll(/([\w-]+)\s*\(/g)].some((fn) => !cssFunctions.has(fn[1].toLowerCase()))
        || !CSS.supports(property, value)) {
        throw new Error(t('不支持的 CSS 声明：') + item)
      }
      return property + ': ' + value
    })
    result.push('.board-free-canvas[data-token-theme] .board-token-card ' + target.selector + '{' + declarations.join(';') + '}')
    offset = rules.lastIndex
  }
  return result.join('\n')
}

export async function loadTokenBoardStyle(): Promise<TokenBoardStyle> {
  const response = await fetch('/api/board-style', { cache: 'no-store' })
  if (!response.ok) throw new Error(t('读取 Token 样式失败。'))
  return response.json()
}

const STYLE_PREVIEW = 'tokenusage:token-style-preview'
const SAVE_DELAY_MS = 250
let saveQueue = Promise.resolve()

function presetCss(preset: TokenBoardStyle['preset']) {
  return presets.find((item) => item.id === preset)?.css ?? presets[0].css
}

export function compileTokenBoardCss(style: TokenBoardStyle) {
  const base = compileTokenCss(presetCss(style.preset))
  if (!style.css || style.css === presetCss(style.preset)) return base
  try { return [base, compileTokenCss(style.css)].join('\n') } catch { return base }
}

function previewInBoard(style: TokenBoardStyle | null, pending: boolean) {
  document.querySelector<HTMLIFrameElement>('.board-preview-frame')?.contentWindow?.postMessage({ type: STYLE_PREVIEW, style, pending }, window.location.origin)
}

export function useTokenBoardStyle() {
  const [style, setStyle] = useState(defaultTokenBoardStyle)
  useEffect(() => {
    let active = true
    let localPreviewPending = false
    let generation = 0
    const apply = (next: TokenBoardStyle) => setStyle((current) => current.preset === next.preset && current.css === next.css ? current : next)
    const sync = async () => {
      if (localPreviewPending) return
      const currentGeneration = generation
      try {
        const next = await loadTokenBoardStyle()
        if (active && generation === currentGeneration) apply(next)
      } catch { /* retain the last applied style during a temporary failure */ }
    }
    const receivePreview = (event: MessageEvent) => {
      if (window.parent === window || event.source !== window.parent || event.origin !== window.location.origin || event.data?.type !== STYLE_PREVIEW) return
      generation += 1
      localPreviewPending = event.data.pending === true
      if (event.data.style) apply(event.data.style)
      if (!localPreviewPending && !event.data.style) void sync()
    }
    window.addEventListener('message', receivePreview)
    void sync()
    const timer = window.setInterval(() => void sync(), 1000)
    return () => {
      active = false
      window.clearInterval(timer)
      window.removeEventListener('message', receivePreview)
    }
  }, [])
  return style
}

export function TokenBoardStyleToolbar() {
  const [open, setOpen] = useState(false)
  const [draft, setDraft] = useState<TokenBoardStyle | null>(null)
  const [invalid, setInvalid] = useState('')
  const [saveError, setSaveError] = useState('')
  const loaded = draft !== null
  const mounted = useRef(false)
  const timer = useRef<number | undefined>(undefined)
  const revision = useRef(0)
  const pending = useRef<{ revision: number; style: TokenBoardStyle } | null>(null)

  const flush = useCallback(() => {
    window.clearTimeout(timer.current)
    const change = pending.current
    if (!change) return
    pending.current = null
    saveQueue = saveQueue.then(async () => {
      if (change.revision !== revision.current) return
      try {
        const response = await fetch('/api/board-style', {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(change.style),
          keepalive: true,
        })
        if (!response.ok) {
          const payload = await response.json().catch(() => ({})) as { error?: string }
          throw new Error(payload.error || t('无法保存样式。'))
        }
        if (mounted.current && change.revision === revision.current) {
          previewInBoard(change.style, false)
        }
      } catch (cause) {
        if (mounted.current && change.revision === revision.current) {
          previewInBoard(null, false)
          setSaveError(t('自动保存失败：') + (cause instanceof Error ? cause.message : t('请重试。')))
        }
      }
    })
  }, [])

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      flush()
    }
  }, [flush])

  useEffect(() => {
    if (!open || loaded) return
    let active = true
    void saveQueue.then(loadTokenBoardStyle).then((style) => {
      if (active) setDraft({ ...style, css: style.css || presetCss(style.preset) })
    }).catch(() => {
      if (active) setSaveError(t('读取 Token 样式失败，请关闭后重试。'))
    })
    return () => { active = false }
  }, [open, loaded])

  function changeDraft(next: TokenBoardStyle, immediate = false) {
    setDraft(next)
    setInvalid('')
    setSaveError('')
    try {
      compileTokenCss(next.css)
    } catch (cause) {
      setInvalid((cause instanceof Error ? cause.message : t('CSS 尚未写完整。')) + ' ' + t('保留上一次有效样式。'))
      return
    }
    const style = { ...next, css: next.css === presetCss(next.preset) ? '' : next.css }
    pending.current = { revision: ++revision.current, style }
    previewInBoard(style, true)
    window.clearTimeout(timer.current)
    if (immediate) flush()
    else timer.current = window.setTimeout(flush, SAVE_DELAY_MS)
  }

  function close() {
    flush()
    setOpen(false)
  }

  const container = useBoardPopoverDismiss(open, close)

  return <div ref={container} className="token-style-toolbar" onKeyDown={(event) => {
    if (event.key === 'Escape' && open) close()
  }}>
    <button type="button" className="button button-quiet token-style-trigger" aria-expanded={open} aria-controls={open ? 'token-style-panel' : undefined} onClick={() => {
      if (open) close()
      else {
        if (!loaded) setSaveError('')
        setOpen(true)
      }
    }}><Palette size={15} /><span>{t('Token 样式')}</span></button>
    {open && <section id="token-style-panel" className="token-style-panel" aria-label={t('Token 样式')}>
      {draft && <>
        <fieldset>
          <legend>{t('预设样式')}</legend>
          <div className="token-style-presets">{presets.map(({ id, label, css }) => <label key={id}>
            <input name="token-style-preset" type="radio" checked={draft.preset === id} onChange={() => changeDraft({ preset: id, css }, true)} />{t(label)}
          </label>)}</div>
        </fieldset>
        <label className="token-style-editor" htmlFor="token-style-css">{t('自定义 CSS')}</label>
        <textarea id="token-style-css" maxLength={4000} spellCheck={false} value={draft.css} onChange={(event) => changeDraft({ ...draft, css: event.target.value })} aria-invalid={invalid ? true : undefined} />
        <p>{t('token-card（卡片）、.token-label（标题）、.token-value（数字）、.token-accent（底线）')}</p>
      </>}
      {!draft && !saveError && <p role="status">{t('正在读取样式…')}</p>}
      {(invalid || saveError) && <p className="token-style-error" role="alert">{invalid || saveError}</p>}
    </section>}
  </div>
}
