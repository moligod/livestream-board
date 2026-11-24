import { t } from './i18n'
import { useMemo } from 'react'
import CalendarHeatmap, { type CalendarHeatmapValue } from 'react-calendar-heatmap'
import 'react-calendar-heatmap/dist/styles.css'
import { formatExact } from './data'
import type { UsageRecord } from './types'

type TokenDay = CalendarHeatmapValue & { count: number; level: number }

function localDateKey(timestamp: number): string {
  const date = new Date(timestamp * 1000)
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`
}

export default function TokenCalendarHeatmap({ records }: { records: UsageRecord[] }) {
  const { values, startDate, endDate, activeDays, yearlyTotal } = useMemo(() => {
    const end = new Date()
    end.setHours(12, 0, 0, 0)
    const start = new Date(end)
    start.setDate(start.getDate() - 364)
    const totals = new Map<string, number>()
    let total = 0
    for (const record of records) {
      if (record.timestamp * 1000 < start.getTime() - 12 * 60 * 60 * 1000) continue
      const key = localDateKey(record.timestamp)
      totals.set(key, (totals.get(key) ?? 0) + record.total_tokens)
      total += record.total_tokens
    }
    const max = Math.max(...totals.values(), 1)
    const days: TokenDay[] = [...totals.entries()].map(([date, count]) => ({
      date,
      count,
      level: count > 0 ? Math.max(1, Math.min(4, Math.ceil((Math.log10(count + 1) / Math.log10(max + 1)) * 4))) : 0,
    }))
    return { values: days, startDate: start, endDate: end, activeDays: days.length, yearlyTotal: total }
  }, [records])
  return <section className="panel token-calendar-panel" aria-label={t('活跃日历')}>
    <div className="overview-visual-meta"><p className="chart-subtitle">{t('颜色越亮，表示当天 Token 消耗越高')}</p><span className="panel-count">{t('{0} 个活跃日', activeDays)} · {formatExact(yearlyTotal)} Token</span></div>
    <div className="token-calendar-scroll">
      <CalendarHeatmap
        startDate={startDate}
        endDate={endDate}
        values={values}
        showWeekdayLabels
        monthLabels={[t('1月'), t('2月'), t('3月'), t('4月'), t('5月'), t('6月'), t('7月'), t('8月'), t('9月'), t('10月'), t('11月'), t('12月')]}
        weekdayLabels={[t('日'), t('一'), t('二'), t('三'), t('四'), t('五'), t('六')]}
        gutterSize={3}
        classForValue={(value) => value ? `token-calendar-day level-${Number(value.level ?? 0)}` : 'token-calendar-day level-0'}
        titleForValue={(value) => value ? `${value.date} · ${formatExact(Number(value.count ?? 0))} Token` : t('无记录')}
      />
    </div>
    <div className="token-calendar-legend"><span>{t('少')}</span>{[0, 1, 2, 3, 4].map((level) => <i className={`level-${level}`} key={level} />)}<span>{t('多')}</span></div>
  </section>
}
