import { useSyncExternalStore } from 'react'
import english from './en-ui.json'

export type Language = 'zh-CN' | 'en'
const STORAGE_KEY = 'tokenusage.language'
function fromStorage(): Language {
  try { return localStorage.getItem(STORAGE_KEY) === 'en' ? 'en' : 'zh-CN' } catch { return 'zh-CN' }
}
let language = fromStorage()
const listeners = new Set<() => void>()
function updateLanguage(next: Language) {
  if (language === next) return
  language = next
  if (typeof document !== 'undefined') document.documentElement.lang = next
  listeners.forEach((listener) => listener())
}
if (typeof window !== 'undefined') {
  document.documentElement.lang = language
  window.addEventListener('storage', (event) => {
    if (event.key === STORAGE_KEY || event.key === null) updateLanguage(fromStorage())
  })
}
function subscribe(listener: () => void) {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}
export function useLanguage() {
  return useSyncExternalStore(subscribe, () => language, () => 'zh-CN' as const)
}
export function setLanguage(next: Language) {
  updateLanguage(next)
  try { localStorage.setItem(STORAGE_KEY, next) } catch { /* current page can still switch */ }
}
export function locale() { return language === 'en' ? 'en-US' : 'zh-CN' }

const translations: Record<string, string> = english
export function t(key: string, ...values: (string | number)[]): string {
  const template = language === 'en' && Object.hasOwn(translations, key) ? translations[key] : key
  return values.length ? template.replace(/\{(\d+)\}/g, (match, index) => String(values[Number(index)] ?? match)) : template
}
