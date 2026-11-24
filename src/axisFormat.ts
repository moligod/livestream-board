const tokenFormatter = new Intl.NumberFormat('en-US', { notation: 'compact', maximumSignificantDigits: 6 })
const costAxisFormatter = new Intl.NumberFormat('en-US', { maximumSignificantDigits: 8 })
const costDetailFormatter = new Intl.NumberFormat('en-US', { maximumSignificantDigits: 12 })

export function formatTokenAxisTick(value: number): string {
  return Number.isFinite(value) ? tokenFormatter.format(value) : '—'
}

function formatCost(value: number, formatter: Intl.NumberFormat): string {
  if (!Number.isFinite(value)) return '—'
  if (value !== 0 && Math.abs(value) < 0.000001) {
    return `$${value.toExponential(4).replace(/\.?0+e/, 'e')}`
  }
  return `$${formatter.format(value)}`
}

export function formatCostAxisTick(value: number): string {
  return formatCost(value, costAxisFormatter)
}

export function formatCostDetail(value: number): string {
  return formatCost(value, costDetailFormatter)
}
