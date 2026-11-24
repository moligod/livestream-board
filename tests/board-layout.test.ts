import assert from 'node:assert/strict'
import test from 'node:test'
import { defaultBoardCards } from '../src/boardLayout.ts'

test('the default desktop board reserves a tall right column for chat', () => {
  for (const [width, height] of [[1280, 720], [1810, 952], [1920, 1080], [2281, 1185], [2560, 1080]]) {
    const cards = defaultBoardCards(width, height)
    assert.equal(cards.length, 7)
    const chat = cards[6]
    assert.ok(chat.x / width > .8 && chat.height > .95 * height)
    assert.equal(chat.y, cards[0].y)
    for (const card of cards.slice(0, 6)) {
      assert.equal(card.y, chat.y)
      assert.ok(card.width > 0 && card.x + card.width < chat.x)
    }
  }
})

test('defaults do not overlap or overflow at desktop and narrow sizes', () => {
  for (const [width, height] of [[1920, 1080], [2267, 1133], [1280, 320], [1024, 768], [390, 844], [320, 300], [1, 1]]) {
    const cards = defaultBoardCards(width, height)
    for (const [i, a] of cards.entries()) {
      assert.ok(a.width > 0 && a.height > 0)
      assert.ok(a.x >= 0 && a.y >= 0 && a.x + a.width <= width && a.y + a.height <= height)
      for (const b of cards.slice(i + 1)) {
        assert.ok(a.x + a.width <= b.x || b.x + b.width <= a.x || a.y + a.height <= b.y || b.y + b.height <= a.y)
      }
    }
    if (width < 1280) assert.ok(cards[6].y > cards[0].y)
  }
})
