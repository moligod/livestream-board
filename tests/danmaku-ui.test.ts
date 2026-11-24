import { test } from 'node:test'
import { strict as assert } from 'node:assert'
import Module from 'node:module'
import { resolve } from 'node:path'
import React from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { build } from 'esbuild'

test('browser source is a display and chat controls stay behind the admin toolbar', async () => {
  const result = await build({ entryPoints: ['src/DanmakuCard.tsx'], bundle: true, platform: 'node', format: 'cjs', jsx: 'automatic', external: ['react', 'react/jsx-runtime', 'lucide-react'], write: false })
  const filename = resolve('danmaku-ui-test.cjs')
  const module = new Module(filename) as Module & { _compile: (code: string, filename: string) => void; paths: string[] }
  module.filename = filename
  module.paths = [resolve('node_modules')]
  module._compile(Buffer.from(result.outputFiles[0].contents).toString('utf8'), filename)
  const { default: Card, DanmakuToolbar } = module.exports as { default: React.ComponentType<Record<string, unknown>>; DanmakuToolbar: React.ComponentType }
  const state = { room_id: 0, real_room_id: 0, status: 'not-configured', authenticated: false, font_size: 16, error: null, revision: 0, messages: [] }
  const render = (value: Record<string, unknown> = state) => renderToStaticMarkup(React.createElement(Card, { state: value }))
  const config = render()
  assert.match(config, /等待新弹幕/)
  assert.doesNotMatch(config, /输入B站直播间号|保存 Cookie|弹幕字号|扫码登录/)
  assert.match(renderToStaticMarkup(React.createElement(DanmakuToolbar)), /B站弹幕设置/)
  const connected = { ...state, room_id: 123, status: 'connected', messages: [{ id: 'test', user: '观众', text: '测试弹幕', timestamp: 1, medal: '', medal_level: 0, user_level: 0 }] }
  const overlay = render(connected)
  assert.match(overlay, /is-audience/)
  assert.doesNotMatch(overlay, /已连接|设置弹幕直播间/)
  assert.match(overlay, /房间 123/)
  assert.match(overlay, /测试弹幕/)
  assert.doesNotMatch(overlay, /输入B站直播间号/)
})
