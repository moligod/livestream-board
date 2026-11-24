# 直播看板（livestream-board）

本地统计 Token 用量，并通过透明网页看板展示 Token 消耗和 B 站文字弹幕。当前支持 Codex、Claude Code、Trae CN、Trae 国际版、OpenCode、Gemini CLI、Grok Build、Pi、Oh My Pi、MiniMax Code、Antigravity、Kimi Code、GitHub Copilot CLI、WorkBuddy 和 DeepSeek Harness，共 15 个本地来源。所有分析均在本机完成。

## 网页端开发

TokenUsage 现在只保留网页端。安装 Node.js、Python 和 Rust 后：

```powershell
python -m pip install -r requirements.txt
npm ci
npm run build
cargo build --release --manifest-path src-tauri/Cargo.toml --bin dashboard-json
python -m tokenusage.web_server
```

然后打开 `http://127.0.0.1:8787`。网页服务会复用已有快照，默认每 60 秒检查各来源变化；扫描频率可在总览的“用量设置”调整。Windows 也可以直接运行 `start-web.cmd` 构建并启动。

侧栏底部可切换简体中文或 English。所选语言保存在当前浏览器内。

## 把看板放进直播画面

- 地址：`http://127.0.0.1:8787/?view=board`。看板网页本身背景透明，仅用于直播展示，不显示设置控件。
- 支持网页源或浏览器源的直播软件直接添加这个地址；不支持网页源的，也可采集已打开的浏览器窗口（不保证保留透明背景）。
- 在管理页「看板」顶部的「B站弹幕设置」填写直播间号；可选扫码登录或填写自己的 Cookie，切勿提交到仓库或发给别人。 登录二维码由项目在本机编码，不依赖二维码第三方库，也不会把登录链接发给外部制码服务。

## 本地隐私与仓库文件

本机用量数据库、会话缓存、备份、凭据、日志、截图工作区、编译产物和依赖目录不随源码发布。服务默认只监听 `127.0.0.1`。使用 B 站弹幕或定价同步时会访问相应服务；不要把本地用量接口直接暴露到公网。

## 同步与模型定价

总览右上角提供“用量设置”和“导出 CSV”。在“用量设置”统一控制自动扫描、同步间隔、立即同步和 Codex 重建。启动或打开总览会检查一次；总览在前台时按所选间隔检查，后台每 10 分钟检查。关闭自动扫描后仍可手动同步。各来源直接从工具原位置读取会话，不集中复制原始文件；统计结果写入本机 SQLite，页面从 SQLite 查询。

用量设置中的“模型定价”可手动增删价格，也可从 models.dev 选择具体供应商和模型后同步。自动定价同步默认关闭，开启后最多每 6 小时更新一次；手动价格不会被同步覆盖。模型选择、价格及同步状态保存在本地 `data/cache/model-pricing.json`，访问 models.dev 时不会上传本机用量数据。

“全选当前结果”覆盖所有匹配的模型，不受列表只显示前 150 条的限制。相同模型 ID 只保留一个供应商：优先保留已选供应商，再选择可识别的官方来源，其余按列表顺序选择；可逐项更换。导入后的定价列表支持搜索和分页。

页面会保留来源记录的正数成本；没有来源成本时，按记录中的模型 ID 匹配当前定价估算。价格单位为美元 / 每百万 Token，缺少模型或所需缓存价格时显示“未定价”，总额只包含已计价记录。当前定价用于估算历史记录，且无法确定实际供应商、套餐与历史阶梯价格，因此不代表实际账单。CSV/JSON 导出仍保留采集器原始成本，不包含页面估算值。

## 快速使用

```powershell
python -m pip install -r requirements.txt
python -m tokenusage report --source all --group-by source
```

命令行的 Codex、Claude Code、OpenCode 与网页端使用同一 Rust 采集器，首次使用前需按上面的步骤构建。可按 `source`、`project`、`model`、`date` 分组：

```powershell
python -m tokenusage report --source codex --group-by model
python -m tokenusage report --source opencode --group-by project
python -m tokenusage report --source all --group-by date --export exports\usage.csv
```

## Trae 数据库

网页服务会自动定位以下本机数据库：

- `%APPDATA%\Trae\ModularData\ai-agent\database.db`：Trae 国际版
- `%APPDATA%\Trae CN\ModularData\ai-agent\database.db`：Trae CN

原始库使用 SQLCipher 加密。保持对应版本 Trae 运行，客户端会在本机进程内存中提取密钥、校验 HMAC，并把临时解密库放在 `%TEMP%\TokenUsage`；密钥不写入磁盘，临时库不会写进项目或 GitHub 仓库。若 Windows 拒绝读取 Trae 进程，请关闭并以管理员身份运行 TokenUsage，同时保持 Trae 运行。

Python CLI 仍支持手动准备解密库。启动对应版本后，在管理员 PowerShell 中运行：

```powershell
python -m tokenusage prepare-trae --edition trae
python -m tokenusage prepare-trae --edition trae-cn
```

默认输出（仅在用户本机执行后生成，不随项目发布）：

- `data\trae.db`：Trae 国际版解密数据库（CLI 手动模式）
- `data\trae-cn.db`：Trae CN 解密数据库（CLI 手动模式）

仓库不会包含任何个人数据库、密钥、聊天记录或认证信息。`data` 目录只作为本机运行时输出目录；用户也可以通过 `--trae-db` / `--trae-cn-db` 指定自己的解密数据库。

默认不保存数据库密钥。如确实需要，可加 `--save-key`，但密钥文件和解密数据库都不得上传或分享。

也可以直接复用已有解密库：

```powershell
python -m tokenusage report --source trae --trae-db C:\path\database_decrypted.db --group-by model
```

## 本地用量存储

- 网页版使用 `data/cache/usage.sqlite` 保存完整的用量明细；统计、趋势和明细分页由本地数据库查询，不需要向页面传输全部历史记录。没有按时间自动删除明细的规则。
- 首次启动会从现有 `data/cache/dashboard.json` 迁入数据，核对记录数和各类 Token 总数；迁移前的快照及会话缓存另存到 `data/backups/migration-*`，原文件也不会被删除。同步会以请求标识去重，来源暂时消失时保留已有历史。
- 运行期间每天检查并创建一致性的 SQLite 备份，保存在 `data/backups/usage-*.sqlite`，默认保留最近 10 份；重建 Codex 用量前也会先生成备份。迁移前副本不参与轮换，数据库中的历史明细不会因此被删除。
- 旧服务尚未重启时，新版页面可以继续读取旧快照。关闭旧服务并重新运行 `start-web.cmd` 后，页面才会使用数据库查询。

## 统计口径

- Codex：按 CC Switch v4.0.25 的会话口径读取 `~/.codex/sessions` 与 `~/.codex/archived_sessions` 的 `.jsonl` 和 `.jsonl.zst`；优先使用精确的 `last_token_usage`，并处理续写分段、缓存读写、重复快照、分支历史和引用式分支。无法安全校验的分支暂缓计入；父日志变化后会重试，等待原因仅写入采集日志；平时只重算发生变化的文件及其依赖。
- “用量设置”中的“重建 Codex 用量”会从仍存在的 rollout 全量重算，并按请求 ID 保留原日志已移除但以前采集过的记录，避免历史随归档清理消失；扫描缓存保存在 `data/cache/codex-rollouts.json`，用量明细保存在本地数据库。重建不读取也不修改 CC Switch 数据库。
- Codex 算法的上游来源与 MIT 许可证见 `THIRD_PARTY_NOTICES.md`。
- Claude Code：读取 `~/.claude/projects` 中主会话和子代理的 assistant 用量，按消息 ID 去重并保留较完整的快照；支持 `CLAUDE_CONFIG_DIR`。日志移除后保留已采集的用量明细，扫描缓存保存在 `data/cache/claude-sessions.json`。
- OBS 看板的布局保存在服务端 `data/cache/board-layout.json`。同一服务上的浏览器源共享位置和尺寸并自动同步；按住 `Ctrl` 可多选卡片，成组移动或缩放。
- B 站弹幕卡片：在管理页「看板」顶部点击「B站弹幕设置」并填写直播间号。默认匿名、只读连接 B 站弹幕 WebSocket，也可在连接框下方粘贴登录 Cookie；后端只保留 `SESSDATA`、`bili_jct`、`DedeUserID`、`buvid` 等登录必需字段，验证登录态后使用 Windows DPAPI 加密到 `%LOCALAPPDATA%\TokenUsage\credentials\bilibili-cookie.dpapi`，前端 API 只返回是否已登录，永不回传 Cookie 原文。连接支持 WBI 签名、登录 UID/buvid 鉴权、心跳、自动重连以及普通/zlib/brotli 数据包；直播间号保存在本机 `data/cache/danmaku-settings.json`。协议行为参考 `Bilibili_Danmuji` 后独立实现，未复制其 GPL-3.0 源码。
- 弹幕字号：设置面板提供 10–32px 字号滑块；点击“应用”后写入 `data/cache/danmaku-settings.json`，不重连弹幕服务，并在多个 OBS 浏览器源间自动同步。用户名和正文使用所选字号，等级与勋章胶囊按比例缩放。
- 弹幕列表：卡片渲染服务端保留的最近 50 条消息，不再固定截取 12 条；卡片高度或字号变化后会重新定位到最新消息，纵向拉长时用更多历史弹幕填满可视区域。
- 在看板预览顶栏点击「看板来源」，可隐藏或显示各时段的 Token 卡片及 B 站弹幕。拖动来源右侧手柄可调整叠放顺序，列表越靠上的卡片越靠前，不改变它的位置和尺寸。设置保存在 `data/cache/board-settings.json`，同步到 OBS 浏览器源。
- OpenCode：读取 SQLite 中已完成的 assistant 消息，按消息时间和模型统计；兼容 V1 `message` 和 V2 `session_message`，包含 V2 已结束的 compaction 用量。
- 范围：当前采集本地会话文件和数据库，不读取 CC Switch 的代理请求日志、按日汇总或定价表。旧日志已删除、仅存在 CC Switch 汇总中的数据，以及其他尚未支持的工具，不会自动进入本项目；金额也不应与 CC Switch 的估算金额直接比较。
- Trae：读取 `chat_turn.context.token_usage`，这是客户端每轮对话口径，不与 `server_history_info.token_usage` 相加。
