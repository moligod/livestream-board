import { locale, t } from './i18n'
import { useBoardPopoverDismiss } from './useBoardPopoverDismiss'
import { useEffect, useRef, useState, type CSSProperties } from 'react'
import { Settings2 } from 'lucide-react'
import { loadDanmakuState, saveDanmakuSettings } from './api'
import type { DanmakuState } from './types'

type SettingsProps = {
  state: DanmakuState
  onLogin?: (newState: DanmakuState) => void
  onSettingsChange: (roomId: number, cookie?: string, clearCookie?: boolean, fontSize?: number) => Promise<void>
}

type LoginResponse = { status?: 'waiting' | 'scanned' | 'expired' | 'authorized'; session_id?: string; image?: string; danmaku?: DanmakuState; error?: string }

async function requestDanmakuLogin(action: 'start' | 'poll' | 'cancel', body: unknown = {}, signal?: AbortSignal, keepalive = false): Promise<LoginResponse> {
  const response = await fetch('/api/danmaku/login/' + action, {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), signal, keepalive,
  })
  const result = await response.json().catch(() => ({})) as LoginResponse
  if (!response.ok) throw new Error(result.error ?? t('B 站登录操作失败，请稍后重试。'))
  return result
}

function DanmakuLoginControls({ authenticated, onLogin }: { authenticated: boolean; onLogin: (state: DanmakuState) => void }) {
  const [qr, setQr] = useState<{ session_id: string; image: string } | null>(null)
  const [busy, setBusy] = useState(false)
  const [status, setStatus] = useState('')
  const [error, setError] = useState('')
  const loginRef = useRef(onLogin)
  useEffect(() => { loginRef.current = onLogin }, [onLogin])

  useEffect(() => {
    if (!qr) return
    const controller = new AbortController()
    const body = { session_id: qr.session_id }
    let timer: number | undefined
    const poll = async () => {
      try {
        const result = await requestDanmakuLogin('poll', body, controller.signal)
        if (controller.signal.aborted) return
        if (result.status === 'authorized' && result.danmaku) {
          loginRef.current(result.danmaku)
          setStatus(t('B站登录已保存。'))
          setQr(null)
        } else if (result.status === 'expired') {
          setStatus(t('二维码已过期，请重新扫码。'))
          setQr(null)
        } else {
          setStatus(result.status === 'scanned' ? t('扫码成功，请在手机上确认。') : t('请用哔哩哔哩 App 扫码。'))
          timer = window.setTimeout(() => void poll(), 1700)
        }
      } catch (reason) {
        if (!controller.signal.aborted) {
          setError(reason instanceof Error ? reason.message : t('扫码登录失败。'))
          setStatus('')
          setQr(null)
        }
      }
    }
    void poll()
    return () => {
      controller.abort()
      window.clearTimeout(timer)
      void requestDanmakuLogin('cancel', body, undefined, true).catch(() => undefined)
    }
  }, [qr])

  const start = async () => {
    setQr(null)
    setBusy(true)
    setError('')
    setStatus(t('正在生成二维码…'))
    try {
      const result = await requestDanmakuLogin('start')
      if (!result.image || !result.session_id) throw new Error(t('没能取得二维码，请重试。'))
      setQr({ image: result.image, session_id: result.session_id })
      setStatus(t('请用哔哩哔哩 App 扫码。'))
    } catch (reason) {
      setStatus('')
      setError(reason instanceof Error ? reason.message : t('扫码登录失败。'))
    } finally { setBusy(false) }
  }

  return <div className="danmaku-login">
    <div className="danmaku-login-actions">
      <button type="button" className="button button-quiet" onClick={() => void start()} disabled={busy}>{qr ? t('重新扫码') : t('扫码登录')}</button>
    </div>
    <small role="status"><strong>{authenticated ? t('已接入 TokenUsage') : t('尚未接入 TokenUsage')}</strong></small>
    <small>{t('打开手机上的哔哩哔哩 App，用「扫一扫」扫描二维码，再在手机上确认登录。无需退出浏览器里的 B 站账号。')}</small>
    {qr && <div className="danmaku-qr">
      <img src={qr.image} alt={t('B站登录二维码')} width={150} height={150} />
      <button type="button" className="button button-quiet" onClick={() => { setQr(null); setStatus('') }}>{t('取消扫码')}</button>
    </div>}
    {status && <p className="danmaku-login-status" role="status">{status}</p>}
    {error && <p className="danmaku-form-error" role="alert">{error}</p>}
  </div>
}

function DanmakuSettingsForm({ state, onLogin, onSettingsChange, onSaved }: SettingsProps & { onSaved?: () => void }) {
  const [roomText, setRoomText] = useState(state.room_id ? String(state.room_id) : '')
  const [cookieText, setCookieText] = useState('')
  const [fontSize, setFontSize] = useState(state.font_size || 16)
  const [saving, setSaving] = useState(false)
  const [formError, setFormError] = useState('')
  useEffect(() => setRoomText(state.room_id ? String(state.room_id) : ''), [state.room_id])
  useEffect(() => setFontSize(state.font_size || 16), [state.font_size])
  const save = async () => {
    const roomId = Number.parseInt(roomText, 10)
    if (!Number.isFinite(roomId) || roomId < 1) return
    setSaving(true)
    setFormError('')
    try { await onSettingsChange(roomId); onSaved?.() } catch (error) { setFormError(error instanceof Error ? error.message : t('保存失败')) } finally { setSaving(false) }
  }
  const saveCookie = async () => {
    const roomId = Number.parseInt(roomText, 10)
    if (!Number.isFinite(roomId) || roomId < 1 || !cookieText.trim()) return
    setSaving(true)
    setFormError('')
    try { await onSettingsChange(roomId, cookieText.trim()); setCookieText(''); onSaved?.() } catch (error) { setFormError(error instanceof Error ? error.message : t('Cookie 保存失败')) } finally { setSaving(false) }
  }
  const clearCookie = async () => {
    const roomId = Number.parseInt(roomText, 10)
    if (!Number.isFinite(roomId) || roomId < 1) return
    setSaving(true)
    setFormError('')
    try { await onSettingsChange(roomId, undefined, true); setCookieText('') } catch (error) { setFormError(error instanceof Error ? error.message : t('Cookie 清除失败')) } finally { setSaving(false) }
  }
  const saveFontSize = async () => {
    const roomId = Number.parseInt(roomText, 10)
    if (!Number.isFinite(roomId) || roomId < 1) return
    setSaving(true)
    setFormError('')
    try { await onSettingsChange(roomId, undefined, false, fontSize) } catch (error) { setFormError(error instanceof Error ? error.message : t('字号保存失败')) } finally { setSaving(false) }
  }
  return <div className="danmaku-config-panel board-interactive" onPointerDown={(event) => event.stopPropagation()}>
      <div className="danmaku-config"><input value={roomText} onChange={(event) => setRoomText(event.target.value.replace(/\D/g, ''))} placeholder={t('输入B站直播间号')} inputMode="numeric" /><button disabled={saving || !roomText} onClick={() => void save()}>{saving ? t('处理中') : t('连接')}</button></div>
      <div className="danmaku-cookie-config"><input type="password" value={cookieText} onChange={(event) => setCookieText(event.target.value)} placeholder={state.authenticated ? t('已保存 Cookie，可替换') : t('粘贴 B站 Cookie（可选）')} autoComplete="off" spellCheck={false} /><button disabled={saving || !cookieText.trim()} onClick={() => void saveCookie()}>{t('保存 Cookie')}</button>{state.authenticated ? <button className="cookie-clear" disabled={saving} onClick={() => void clearCookie()}>{t('清除')}</button> : null}</div>
      <div className="danmaku-font-size-control"><label htmlFor="danmaku-font-size">{t('弹幕字号') + ' '}<output>{fontSize}px</output></label><input id="danmaku-font-size" type="range" min="10" max="32" step="1" value={fontSize} onChange={(event) => setFontSize(Number(event.target.value))} /><button disabled={saving || fontSize === state.font_size} onClick={() => void saveFontSize()}>{t('应用')}</button></div>
      {onLogin && <DanmakuLoginControls authenticated={state.authenticated} onLogin={(newState) => { setCookieText(''); onLogin(newState) }} />}
      {formError ? <p className="danmaku-form-error">{formError}</p> : null}
  </div>
}

export function DanmakuToolbar() {
  const [open, setOpen] = useState(false)
  const [state, setState] = useState<DanmakuState | null>(null)
  const [loadError, setLoadError] = useState(false)
  const triggerRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    if (!open) return
    let active = true
    void loadDanmakuState().then((value) => {
      if (active) setState(value)
    }).catch(() => {
      if (active) setLoadError(true)
    })
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      setOpen(false)
      triggerRef.current?.focus()
    }
    document.addEventListener('keydown', closeOnEscape)
    return () => {
      active = false
      document.removeEventListener('keydown', closeOnEscape)
    }
  }, [open])

  const container = useBoardPopoverDismiss(open, () => setOpen(false))

  return <div ref={container} className="danmaku-toolbar">
    <button ref={triggerRef} type="button" className="button button-quiet danmaku-toolbar-trigger" aria-expanded={open} aria-controls={open ? 'danmaku-toolbar-panel' : undefined} onClick={() => {
      if (!open) {
        setState(null)
        setLoadError(false)
      }
      setOpen(!open)
    }}><Settings2 size={15} /><span>{t('B站弹幕设置')}</span></button>
    {open && <section id="danmaku-toolbar-panel" className="danmaku-toolbar-panel" aria-label={t('B站弹幕设置')}>
      {state ? <DanmakuSettingsForm state={state} onLogin={setState} onSettingsChange={async (roomId, cookie, clearCookie, fontSize) => {
        setState(await saveDanmakuSettings(roomId, cookie, clearCookie, fontSize))
      }} /> : <small role="status">{loadError ? t('读取弹幕设置失败，请关闭后重试。') : t('正在加载弹幕设置…')}</small>}
    </section>}
  </div>
}

export default function DanmakuCard({ state }: { state: DanmakuState }) {
  const messagesRef = useRef<HTMLDivElement>(null)
  const statusText = state.status === 'connected' ? t('已连接') : state.status === 'connecting' ? t('连接中') : state.status === 'reconnecting' ? t('重连中') : state.status === 'disconnected' ? t('已断开') : t('未配置')
  const messages = state.messages
  const latestMessageId = messages.at(-1)?.id
  useEffect(() => {
    const element = messagesRef.current
    if (!element) return
    let frame = 0
    const scrollToLatest = () => {
      window.cancelAnimationFrame(frame)
      frame = window.requestAnimationFrame(() => { element.scrollTop = element.scrollHeight })
    }
    scrollToLatest()
    const observer = new ResizeObserver(scrollToLatest)
    observer.observe(element)
    if (element.lastElementChild) observer.observe(element.lastElementChild)
    return () => { window.cancelAnimationFrame(frame); observer.disconnect() }
  }, [latestMessageId, messages.length, state.font_size])
  return <div className="danmaku-card is-audience" style={{ '--danmaku-font-size': `${state.font_size || 16}px` } as CSSProperties}>
    <div className="danmaku-messages" ref={messagesRef} aria-live="polite">
      {messages.length ? messages.map((message) => <div className="danmaku-row" key={message.id}><div className="danmaku-user">{message.user_level > 0 ? <span className="danmaku-level">Lv.{message.user_level}</span> : null}{message.medal ? <span className="danmaku-medal">{message.medal_level} {message.medal}</span> : null}<strong>{message.user}:</strong></div><p>{message.text}</p><time>{new Date(message.timestamp * 1000).toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' })}</time></div>) : <div className="danmaku-empty">{state.room_id ? statusText === t('已连接') ? t('等待新弹幕…') : state.error || statusText : t('等待新弹幕…')}</div>}
    </div>
    {state.room_id > 0 ? <div className="danmaku-room">{t('房间') + ' '}{state.real_room_id || state.room_id}</div> : null}
  </div>
}

