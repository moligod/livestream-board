import { locale, t } from './i18n'
import { useMemo } from 'react'
import { Area, CartesianGrid, ComposedChart, Legend, Line, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts'
import { formatExact } from './data'
import { formatCostAxisTick, formatCostDetail, formatTokenAxisTick } from './axisFormat'
import { buildUsageTrend, granularityLabels } from './usageTrend'
import type { UsageRecord } from './types'
import type { TimeWindow } from './usageTrend'

export default function HourlyUsageLinePanel({ records, timeWindow }: { records: UsageRecord[]; timeWindow: TimeWindow }) {
  const { points, granularity, invalidRange } = useMemo(
    () => buildUsageTrend(records, timeWindow),
    [records, timeWindow.start, timeWindow.end],
  )
  const granularityLabel = t(granularityLabels[granularity])
  return <section className="panel hourly-line-panel" aria-label={t('用量趋势')}>
    <div className="overview-visual-meta"><span className="panel-count">{granularityLabel} · {t('{0} 个区间', points.length)}</span></div>
    {points.length ? <div className="hourly-line-chart" role="img" aria-label={t('{0} Token 用量折线图', granularityLabel)}>
      <ResponsiveContainer width="100%" height="100%">
        <ComposedChart data={points} margin={{ top: 18, right: 14, bottom: 0, left: 0 }}>
          <CartesianGrid stroke="#26323b" strokeDasharray="4 4" vertical={false} />
          <XAxis dataKey="label" tick={{ fill: '#71808b', fontSize: 11 }} axisLine={{ stroke: '#34434c' }} tickLine={false} minTickGap={36} />
          <YAxis yAxisId="tokens" tickFormatter={formatTokenAxisTick} tick={{ fill: '#71808b', fontSize: 11 }} axisLine={false} tickLine={false} allowDecimals={false} width="auto" />
          <YAxis yAxisId="cost" orientation="right" tickFormatter={formatCostAxisTick} tick={{ fill: '#71808b', fontSize: 11 }} axisLine={false} tickLine={false} width="auto" />
          <Tooltip contentStyle={{ color: '#dce5e8', border: '1px solid #34434c', borderRadius: 8, background: '#111a21', boxShadow: '0 12px 32px rgba(0, 0, 0, .32)' }} labelStyle={{ color: '#e1e9ec', fontWeight: 700 }} labelFormatter={(label, payload) => { const point = payload[0]?.payload; return typeof point?.intervalStart === 'number' && typeof point?.intervalEnd === 'number' ? `${new Date(point.intervalStart * 1000).toLocaleString(locale(), { hour12: false })} — ${new Date(point.intervalEnd * 1000).toLocaleString(locale(), { hour12: false })}` : String(label) }} formatter={(value, name) => [name === t('成本') ? formatCostDetail(Number(value)) : formatExact(Number(value)), name]} />
          <Legend iconType="plainline" wrapperStyle={{ color: '#7d8b94', fontSize: 11, paddingTop: 8 }} />
          <Area yAxisId="tokens" type="monotone" dataKey="total" name={t('总量')} stroke="#9b51ff" strokeWidth={2.5} fill="#9b51ff" fillOpacity={0.12} dot={points.length === 1} activeDot={{ r: 4 }} isAnimationActive={false} />
          <Line yAxisId="cost" type="monotone" dataKey="cost" name={t('成本')} stroke="#ff4267" strokeWidth={2} strokeDasharray="5 5" dot={points.length === 1} isAnimationActive={false} />
          <Line yAxisId="tokens" type="monotone" dataKey="cache" name={t('缓存命中')} stroke="#ff8800" strokeWidth={2} dot={points.length === 1} isAnimationActive={false} />
          <Line yAxisId="tokens" type="monotone" dataKey="input" name={t('输入')} stroke="#2d8cff" strokeWidth={2} dot={points.length === 1} isAnimationActive={false} />
          <Line yAxisId="tokens" type="monotone" dataKey="output" name={t('输出')} stroke="#12b981" strokeWidth={2} dot={points.length === 1} isAnimationActive={false} />
        </ComposedChart>
      </ResponsiveContainer>
    </div> : <div className="empty-state line-empty">{invalidRange ? t('开始时间不能晚于结束时间') : t('当前范围暂无趋势数据')}</div>}
  </section>
}
