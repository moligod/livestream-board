export type DateTimeParts = { date: string; time: string }

const pad = (value: number) => String(value).padStart(2, '0')

/** Format in the user's local timezone, as required by the date and time inputs. */
export function localDateTimeParts(timestamp: number): DateTimeParts {
  const date = new Date(timestamp * 1000)
  return {
    date: `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`,
    time: `${pad(date.getHours())}:${pad(date.getMinutes())}`,
  }
}

/** Return null for incomplete or nonexistent local times, including DST gaps. */
export function parseLocalDateTime(date: string, time: string): number | null {
  const dateMatch = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date)
  const timeMatch = /^(\d{2}):(\d{2})$/.exec(time)
  if (!dateMatch || !timeMatch) return null
  const [, yearText, monthText, dayText] = dateMatch
  const [, hourText, minuteText] = timeMatch
  const year = Number(yearText)
  const month = Number(monthText)
  const day = Number(dayText)
  const hour = Number(hourText)
  const minute = Number(minuteText)
  if (year < 1) return null
  const result = new Date(0)
  result.setFullYear(year, month - 1, day)
  result.setHours(hour, minute, 0, 0)
  if (result.getFullYear() !== year || result.getMonth() !== month - 1 || result.getDate() !== day || result.getHours() !== hour || result.getMinutes() !== minute) return null
  return Math.floor(result.getTime() / 1000)
}

/** A Sunday-first six-week grid, including adjacent months. */
export function calendarMonthDays(year: number, month: number): { date: string; day: number; inMonth: boolean }[] {
  const offset = new Date(year, month, 1, 12).getDay()
  return Array.from({ length: 42 }, (_, index) => {
    const date = new Date(year, month, 1 - offset + index, 12)
    return {
      date: localDateTimeParts(date.getTime() / 1000).date,
      day: date.getDate(),
      inMonth: date.getMonth() === month,
    }
  })
}
