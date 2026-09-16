# 功能详情

当前已实现功能的精确行为与口径。改功能前先核对此文档（它应与代码同步更新）；用户视角的说明见 `README.md`。

## 数据与统计口径

- **今日总量** = `input + output + reasoning + cache_creation`（与 ZCode 官方统计同口径）；**缓存命中 cache_read 是提示复用，不计入总量**，以命中率展示：`cache_read ÷ (input + cache_creation + cache_read)`。
- **平均速度 avg_tps** = Σ(output+reasoning) ÷ Σ纯生成时长；分母用 `completed_at - first_token_at`（排除首 token 等待/排队），`first_token_at` 缺失退化为 `duration_ms`，下限 50ms。
- 今日归属按调用完成时刻，跨天自动清零（rollover）。
- 数据源：`~/.zcode/cli/db/db.sqlite` 的 `model_usage` 表，只读打开（WAL 不影响运行中的 CLI）。

## 实时速度（liveio.rs）

- 30s 滑窗 ∩ 活跃段的清洗管道字节率 ÷ 自校准系数（字节→token）。
- 状态来源 `live_source`：`io`（实测流式）→ `window`（门控判定生成中但管道静默，显示近期已完成调用的真实速度，前端加 ≈ 标记）→ `idle`（归零）。
- 首字节前（TTFT）显示"统计中…"；流式启停秒级响应（门控：message 表 assistant 行 vs 完成行）。
- 系数冷启动为 600 先验，完成 1~2 个 ≥300 token 的调用后收敛；管道静默调用不入样（见 key-rules #5）。
- 精度：达标调用 `pred/true` 应在 0.8~1.25（实测 1.00~1.05）；对账命令 `python scripts/live_vs_true.py`。

## 仪表与曲线（gauges.ts）

- **当前速度表**：最小量程 60 t/s（`minScale` 可按表覆盖）；**平均速度表**：最小 10 t/s；**今日总量表**：最小量程 1 亿 token，超峰值后自动放大（1亿→2亿→5亿→…），回落缓慢收缩。
- **当前速度表分档配色**（`gauges.ts` 顶部 `SPEED_TIERS`，主表与迷你仪表共用）：背景轨道恒灰，整条进度弧随当前速度所在档**整体**换色——0–30 绿 `#34d399` / 30–60 黄 `#fbbf24` / 60+ 红 `#f87171`，大数字同步变色；待机（0）数字保持默认白，估算态仍为琥珀 ≈，不走分档。
- 量程跟随峰值平滑变化（峰值上涨立即放大、回落指数收敛），估算时指针/弧线变琥珀色并加 ≈。
- **15 分钟速度曲线**：后端 90 桶 × 10s（对齐墙钟边界），前端按 `nowMs % 10s` 相位连续左移；横轴标注**真实墙钟时刻**（每 5 分钟整分刻度 + 右缘当前时刻），可与数据直接对表。

## 悬浮窗与桌宠

- 三形态：**桌宠**（默认 200×200，默认宠物鲸鱼女仆 maid-deepseek-whale）/ **迷你仪表**（116×116）/ **速度胶囊**（224×78）。完整面板右上角**自绘下拉**切换（深色弹层，点击选项/外部、Esc、窗口失焦均关闭；原生 `<select>` 因 WebView2 弹层跟随系统浅色主题不可读而弃用，见 key-rules #8），选择持久化。
- 桌宠：精灵动画（生成中跑步/待机站立），头顶气泡只显示 t/s（无状态文字，描边色区分状态）；**滚轮上下缩放** 100~480 逻辑像素（`set_float_size` 持久化）；双击或 🔄 按钮换宠物（存储键 `petPack.v2`）。
- 悬浮窗/桌宠**右键菜单**：恢复窗体 / 退出程序（`quit_app` 命令）。
- **位置独立记忆**：完整面板与悬浮窗各自记住位置（`~/.zcode/speed-panel-mode.txt` JSON 的 `full_pos`/`float_pos`/`pet_size`，物理像素）；收起时悬浮窗回自己上次位置（无记忆则锚定当前窗体中心），展开时窗体回自己老位置，均 `clamp_to_screen` 防止跑出屏幕。拖动期间位置落盘节流 2s，关闭/退出立即落盘。
- 整块可拖动（`startDragging`；按钮/下拉框不参与）。

## 窗口与托盘

- **无边框窗口**（`decorations:false`）+ 自绘顶栏（左侧为应用图标 `app-icon.png`）：`#app-header` 带原生 `data-tauri-drag-region`，空白处拖动由内核处理，子元素经 enableDrag 冒泡拖动（二者互斥，按钮/输入/`.dropdown` 不参与）；双击顶栏最大化/还原。
- 顶栏控制：**— 最小化**（`core:window:allow-minimize`）、**▢ 最大化/还原**（`allow-toggle-maximize`）、**✕ = 收起为悬浮窗**（不是退出；完全退出走托盘菜单或悬浮窗右键菜单）。
- 托盘：左键单击显示/隐藏；右键菜单（显示面板 / 隐藏到托盘 / 悬浮窗切换 / 退出）。重复启动唤起已有窗口（single-instance 插件）。
- 窗口标题实时同步当前速度（任务栏/Alt+Tab 可见）。

## 日志系统（main.rs DebugLog）

- `~/.zcode/speed-panel-debug.jsonl`：JSONL 追加写，8MB 轮转为 `.jsonl.1`；轮转旧文件超 7 天在启动时自动删除。
- 事件格式与排查方法见 [key-rules.md](key-rules.md) #6。

## 持久化文件

| 文件 | 内容 |
|---|---|
| `~/.zcode/speed-panel-mode.txt` | JSON：mode/style/full_pos/float_pos/pet_size（旧格式纯文本兼容） |
| `~/.zcode/speed-panel-debug.jsonl` | 调试日志（8MB 轮转 + 7 天清理） |

## CI 与发布

- `.github/workflows/build.yml`：**不随普通推送自动触发**；`v*` 标签 → 自动创建 GitHub Release 附 NSIS 安装包；`workflow_dispatch` → Actions 页手动触发，产物在本次运行的 Artifacts。
- 本地正式版：`npm run tauri build` → `src-tauri/target/release/bundle/nsis/*.exe`。

## 已知边界情况

- 管道静默调用（实测约半数）实时读数走 ≈ 回退，属预期行为而非 bug。
- 多调用并发（子 agent）时实时只统计"当前会话"（最近完成调用的会话）对应的 CLI 进程。
- 冷启动后系数需 1~2 个达标调用收敛，此前读数可能有偏差。
