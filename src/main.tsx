import { t } from './i18n'
import { StrictMode, Component, type ErrorInfo, type ReactNode } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App'
import './styles.css'

class AppErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null }
  static getDerivedStateFromError(error: Error) { return { error } }
  componentDidCatch(error: Error, info: ErrorInfo) { console.error('TokenUsage render error', error, info) }
  render() {
    if (this.state.error) return <div className="boot-error"><strong>{t('TokenUsage 加载失败')}</strong><span>{this.state.error.message}</span><button onClick={() => location.reload()}>{t('重新加载')}</button></div>
    return this.props.children
  }
}

const root = document.getElementById('root')!
root.replaceChildren()
createRoot(root).render(<StrictMode><AppErrorBoundary><App /></AppErrorBoundary></StrictMode>)
