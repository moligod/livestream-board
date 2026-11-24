import type { UsageSummary } from './types'

type LedgerWindow = Pick<UsageSummary, 'page' | 'latest_record_id'> & { records: readonly { row_id?: number }[] }
const noArrivals: ReadonlySet<number> = new Set()

export function arrivingLedgerRows(previous: { scope: string | null; summary: LedgerWindow } | null, scope: string | null, next: LedgerWindow): ReadonlySet<number> {
  const before = previous?.summary.latest_record_id
  const latest = next.latest_record_id
  // The high-water mark belongs to the whole ledger, not the visible filters.
  // A row becoming visible due to an edit or range change isn't an insertion.
  if (scope === null || previous?.scope !== scope || next.page !== 1 ||
      before === undefined || latest === undefined || latest <= before) return noArrivals
  return new Set(next.records.flatMap(({ row_id }) => row_id !== undefined && row_id > before ? [row_id] : []))
}
