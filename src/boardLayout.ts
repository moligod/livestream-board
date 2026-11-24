import type { BoardCardGeometry } from './types.ts'

/** A clean browser source starts with six metrics on the left and a tall chat on the right. */
export function defaultBoardCards(width: number, height: number): BoardCardGeometry[] {
  width = Math.max(1, width)
  height = Math.max(1, height)
  const x = Math.min(Math.max(4, Math.round(width * .003)), width / 8)
  const y = Math.min(Math.max(8, Math.round(height * .014)), height / 8)
  const gap = Math.min(Math.max(6, Math.round(width * .004)), width / 12, height / 12)
  const usableWidth = width - 2 * x
  const usableHeight = height - 2 * y
  const preferredHeight = Math.min(104, Math.max(80, Math.round(height * .086)))
  if (width >= 1280) {
    const chatWidth = Math.round(width * .164)
    const leftWidth = usableWidth - chatWidth - 2 * gap
    const cardWidth = (leftWidth - 5 * gap) / 6
    const cards = Array.from({ length: 6 }, (_, index) => ({
      x: x + index * (cardWidth + gap), y, width: cardWidth, height: Math.min(preferredHeight, usableHeight),
    }))
    return [...cards, { x: width - x - chatWidth, y, width: chatWidth, height: usableHeight }]
  }
  const columns = Math.max(1, Math.min(6, Math.floor((usableWidth + gap) / (160 + gap))))
  const rows = Math.ceil(6 / columns)
  const cardWidth = (usableWidth - (columns - 1) * gap) / columns
  const cardHeight = Math.min(preferredHeight, (usableHeight - rows * gap) / (rows + 1))
  const cards = Array.from({ length: 6 }, (_, index) => ({
    x: x + (index % columns) * (cardWidth + gap),
    y: y + Math.floor(index / columns) * (cardHeight + gap), width: cardWidth, height: cardHeight,
  }))
  const chatY = y + rows * (cardHeight + gap)
  return [...cards, { x, y: chatY, width: usableWidth, height: height - y - chatY }]
}
