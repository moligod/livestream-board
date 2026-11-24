declare module 'react-calendar-heatmap' {
  import type { ComponentType, ReactElement, SVGProps } from 'react'
  export type CalendarHeatmapValue = { date: string; count?: number; [key: string]: unknown }
  export type CalendarHeatmapProps = {
    startDate: Date | string | number
    endDate: Date | string | number
    values: CalendarHeatmapValue[]
    showWeekdayLabels?: boolean
    showMonthLabels?: boolean
    monthLabels?: string[]
    weekdayLabels?: string[]
    gutterSize?: number
    classForValue?: (value: CalendarHeatmapValue | null) => string
    titleForValue?: (value: CalendarHeatmapValue | null) => string
    transformDayElement?: (element: ReactElement<SVGProps<SVGRectElement>>, value: CalendarHeatmapValue | null, index: number) => ReactElement
  }
  const CalendarHeatmap: ComponentType<CalendarHeatmapProps>
  export default CalendarHeatmap
}
