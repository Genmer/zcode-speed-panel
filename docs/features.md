# 功能详情

当前已实现功能的精确行为与口径。改功能前先核对此文档（它应与代码同步更新）；用户视角的说明见 `README.md`。

## 数据与统计口径

- **今日总量** = `input + output + reasoning + cache_creation`（与 ZCode 官方统计同口径）；**缓存命中 cache_read 是提示复用，不计入总量**，以命中率展示：`cache_read ÷ (input + cache_creation + cache_read)`。
- **平均速度 avg_tps** = Σ(output+reasoning) ÷ Σ纯生成时长；分母用 `completed_at - first_token_at`（排除首 token 等待/排队），`first_token_at` 缺失退化为 `duration_ms`，下限 50ms。
- 今日归属按调用完成时刻，跨天自动清零（rollover）。
- 数据源：`~/.zcode/cli/db/db.sqlite` 的 `model_usage` 表，只读打开（WAL 不影响运行中的 CLI）。

## 实时速度（liveio.rs）

- 30s 滑窗 ∩ 活跃段的清洗管道字节率 ÷ 自校准系数（字节→token）。
- **平台原语**（`liveio::platform`，按 cfg 三份实现：win / mac / stub，examples 复用同一路径）：
  - **Windows**：Toolhelp32 枚举 + `zcode.exe` 命令行含 `zcode.cjs` 过滤；`GetProcessIoCounters` 的 `WriteTransferCount` 累计写字节；tracked 文件（rollout/日志/WAL）总量扫描供落盘扣除。
  - **macOS**：`proc_listallpids` 枚举 + `KERN_PROCARGS2` 命令行参数精确匹配 `zcode-cli`（CLI 由 Electron Helper fork 而来，可执行路径与其他 Helper 相同，proc_pidpath 无法区分，实测 CLI 进程参数区有独立的 `zcode-cli` 串）；`proc_pid_rusage(RUSAGE_INFO_V4)` 的 `ri_diskio_byteswritten` 累计磁盘写字节，句柄持有打开时抓取的 `ri_proc_start_abstime` 防 pid 复用（不一致视为进程退出剔除）；**不做 tracked 文件扣除**——实测 rollout 目录 du 净变化可为负（CLI 清理轮转），负增量会反噬清洗流，且恒 0 免去每拍目录扫描。rusage_info_v4 为逐字段 `#[repr(C)]` 镜像，关键字段偏移（start_abstime=80、diskio_byteswritten=152）用 `offset_of!` 编译期断言钉死，SDK 布局变化直接编译失败。
  - **其他平台**：stub（空列表/None），面板回退窗口/估算显示。
- **CleanParams 平台参数表**（清洗/校准参数化，`CleanParams::platform()` 启动时锁定；Windows 列为长期实测原值禁改，mac 列为 120s 探针实测初值**须实测复核**）：

  | 参数 | Windows | macOS | 原因 |
|---|---|---|---|
  | burst_tick_bytes（突发剔除） | 100_000 | u64::MAX（禁用） | mac 流式即单拍突发（225KB~1.5MB 常态），100KB 阈值会丢弃全部信号 |
  | base_noise_bps（静态底噪） | 3_000 | 0 | mac idle 实测 17s 严格 0 字节 |
  | floor_cap_bytes（自适应底噪封顶） | 2_000 | 2_000 | 封顶只防毒化；mac idle 恒 0 时自适应自行降 0 |
  | default_bpt（系数先验） | 600 | 700 | mac 真值对账（2026-09-17，6 条 cal 事件）接受样本 614/724；旧值 2000 源自探针误判，冷启动 3 倍低估 |
  | cal_min / cal_max（样本区间） | 100 / 6_000 | 100 / 12_000 | mac 覆盖对账实测 ~650 留余量 |
  | cal_min_tokens（样本门槛） | 300 | 300 | 平台无关 |
  | detect_ms（锚点探测窗） | 2_500 | 2_500 | 首版不动；mac 若状态抖动再调 5_000 |
  | cal_grace_ms（延迟落盘宽限） | 0（当拍处理） | 15_000 | mac 的 ri_diskio_byteswritten 是页缓存异步落盘计数，滞后 write() 数秒~数十秒（实测 117s 调用 96% 字节落在 completed 后）；调用完成后等满宽限再积分，校准积分与 raw 统计窗口上限同步延长到 completed+grace，分子分母同口径 |
  | cal_outlier_ratio（样本离群拒绝） | 0（禁用） | 3.0 | 样本 B/token 与当前生效系数偏差超 3 倍即拒收：延迟落盘的半截样本（实测 186 偏低入队污染中位数）与归因异常样本不进中位数 |

- **启停门控**：usage 库 message 表的 assistant 消息行——调用开始瞬间提交（≤200ms 可读），行内 `time.completed` 在结束（**含取消/出错**）瞬间补写。扫描最近活跃会话（`session.time_updated` 倒序前 6 个，各自走 `(session_id, time_created)` 复合索引取最新 assistant 行）的最新 assistant 行：未带 `completed` 且 10 分钟内 → 进行中（**不限会话**，新开对话首个调用当拍即亮）；带 `completed` → 当拍归零。已归属会话的 CLI 进程退出时强制判停（崩溃后无人补写 `completed` 的僵尸行兜底）。
- **启动提示（is_starting）**：门控已开但首字节未到（TTFT，20s 窗口内）→ 表盘/迷你仪表/胶囊/桌宠气泡显示 **"…"**（青色呼吸脉冲弧），不显示估算值；超窗仍无字节 → 视为管道静默调用，回退 ≈ 估算。
- 状态来源 `live_source`：`io`（实测流式）→ `window`（门控判定生成中但管道静默，显示近期已完成调用的真实速度，前端加 ≈ 标记）→ `idle`（归零）。
- **IO 不可用时**（进程从未发现：探测环境不可用/刚启动）按门控显示估算或"统计中"，而不是按调用间隔盲估；**CLI 全部退出后**立即归零（旧行为按间隔中位数可空转"估算中"最长 240s）。
- 首字节后读数当拍可用（此前为 TTFT "…" 提示）；进程发现：常驻 30s 刷新，无任何进程时缩短到 2s（新开 CLI 快速可见）。
- 系数冷启动为 600（mac 700）先验，完成 1~2 个 ≥300 token 的调用后收敛；管道静默调用不入样（见 key-rules #5）。
- **延迟落盘宽限与离群拒绝（mac）**：调用完成后 pending 校准事件等满 15s 再处理（`cal_grace_ms`，Windows=0 当拍处理），校准积分与 raw 统计窗口上限同步延长到 `completed + 15s`（`stream_start` 下限不变，仍为 completed − min(gen_ms, 300s)；pred_tps 口径不变，分母仍用真实 gen_ms）；样本 B/token 与当前生效系数偏差超 3 倍即拒收（`cal_outlier_ratio`，Windows=0 禁用），防延迟落盘半截样本与归因异常样本污染中位数。cal 日志新增 `attr_pid`（clean 积分实际用的进程，全进程求和分支为 null）与 `top_pid`（raw 最大进程）供归因异常定位。
- 精度：达标调用 `pred/true` 应在 0.8~1.25（实测 1.00~1.05）；对账命令 `python scripts/live_vs_true.py`。

## 仪表与曲线（gauges.ts）

- **当前速度表**：最小量程 60 t/s（`minScale` 可按表覆盖）；**平均速度表**：最小 10 t/s；**今日总量表**：最小量程 1 亿 token，超峰值后自动放大（1亿→2亿→5亿→…），回落缓慢收缩。
- **当前速度表分档配色**（`gauges.ts` 顶部 `SPEED_TIERS`，主表与迷你仪表共用）：背景轨道恒灰，整条进度弧随当前速度所在档**整体**换色——0–30 绿 `#34d399` / 30–60 黄 `#fbbf24` / 60+ 红 `#f87171`，大数字同步变色；待机（0）数字保持默认白，估算态仍为琥珀 ≈，不走分档。
- 量程跟随峰值平滑变化（峰值上涨立即放大、回落指数收敛），估算时指针/弧线变琥珀色并加 ≈；**启动期（is_starting）数字显示 "…" 并以青色短弧呼吸脉冲**（已连接、等待首字节），不走分档色。
- **15 分钟速度曲线**：后端 90 桶 × 10s（对齐墙钟边界），前端按 `nowMs % 10s` 相位连续左移；横轴标注**真实墙钟时刻**（每 5 分钟整分刻度 + 右缘当前时刻），可与数据直接对表。

## 悬浮窗与桌宠

- 三形态：**桌宠**（默认 200×200，默认宠物鲸鱼女仆 maid-deepseek-whale）/ **迷你仪表**（116×116）/ **速度胶囊**（224×78）。完整面板右上角**自绘下拉**切换（深色弹层，点击选项/外部、Esc、窗口失焦均关闭；原生 `<select>` 因 WebView2 弹层跟随系统浅色主题不可读而弃用，见 key-rules #8），选择持久化。
- 桌宠：精灵动画（生成中/启动等待跑步、待机站立），头顶气泡显示 t/s（启动期显示 "…"，无状态文字，描边色区分状态）；**滚轮上下缩放** 100~480 逻辑像素（`set_float_size` 持久化）；双击或 🔄 按钮换宠物（存储键 `petPack.v2`）。
- 悬浮窗/桌宠**右键菜单**：恢复窗体 / 退出程序（`quit_app` 命令）。
- **位置独立记忆**：完整面板与悬浮窗各自记住位置（`~/.zcode/speed-panel-mode.txt` JSON 的 `full_pos`/`float_pos`/`pet_size`，物理像素）；收起时悬浮窗回自己上次位置（无记忆则锚定当前窗体中心），展开时窗体回自己老位置，均 `clamp_to_screen` 防止跑出屏幕。拖动期间位置落盘节流 2s，关闭/退出立即落盘。
- 整块可拖动（`startDragging`；按钮/下拉框不参与）。

## 窗口与托盘

- **无边框窗口**（`decorations:false`，透明窗口在 mac 走 `macos-private-api` + `macOSPrivateApi`）+ 自绘顶栏（应用图标 `app-icon.png`）：`#app-header` 带原生 `data-tauri-drag-region`，空白处拖动由内核处理，子元素经 enableDrag 冒泡拖动（二者互斥，按钮/输入/`.dropdown` 不参与）；双击顶栏安全最大化/还原。
- 顶栏控制平台原生化：
  - **macOS**：控制按钮移至顶栏最左侧，为原生交通灯圆点（左起依次为：红 `#ff5f56` 收起为悬浮窗、黄 `#ffbd2e` 最小化、绿 `#27c93f` 安全最大化），平时半透明纯色圆点，鼠标悬停控制区时显现微小符号（`✕`、`—`、`▢`）；
  - **Windows**：保持顶栏最右侧自绘 `— 最小化`、`▢ 最大化`、`✕ 收起为悬浮窗` 风格不变。
  - 完全退出走托盘菜单或悬浮窗右键菜单。
- **多屏安全最大化（`toggle_maximize_safe`）**：macOS 无边框窗口调用系统 `toggleMaximize()` 会触发系统 `zoom:` 回退到主屏跳屏。后端通过 `toggle_maximize_safe` 计算窗口中心点所在显示器（`monitor_from_point`）铺满（避让顶部菜单栏 28pt），并记忆还原物理矩形；再次调用或双击顶栏安全还原至原副屏位置和尺寸；折叠为悬浮窗时清理暂存。
- 托盘：左键单击显示/隐藏；右键菜单**顶部为实时状态行**（disabled 不可点，poller 每拍按快照更新：生成中 `x.x t/s` / 估算中 `≈x.x t/s` / 待机；文本变化才写入，托盘 tooltip 同步为 `ZCode 速度仪表盘 · 状态`），其后是菜单项（显示面板 / 隐藏到托盘 / 悬浮窗切换 / 退出）。重复启动唤起已有窗口（single-instance 插件）。
- 窗口标题实时同步当前速度（任务栏/Alt+Tab 可见）。
- **mac 差异**：
  - 应用为 **Accessory 模式**（`set_activation_policy`，setup 内尽早调用）：无 Dock 图标、不进 Cmd+Tab，常驻菜单栏托盘。
  - 自定义应用菜单：`Cmd+Q` 被拦截为"隐藏为悬浮窗"（菜单中**不含任何系统退出项**，保证退出只走托盘与悬浮窗右键）；附"编辑" submenu（cut/copy/paste/select_all）保住 WebView 的 Cmd+C/V/X/A。
  - 退出兜底：`RunEvent::ExitRequested { code: None }` 一律 `prevent_exit` + 保存 + 折叠为悬浮窗（真退出 `app.exit(0)` 时 code=Some 放行，`RunEvent::Exit` 再保存一次）。**真退出只有托盘菜单"退出"与悬浮窗右键"退出程序"两条路**。
  - **引导提示**：前端右上角显示"应用常驻菜单栏 ↗ 点菜单栏图标可显示面板 / 退出"（深色半透明、顶部小箭头指向菜单栏），6 秒自动淡出、点击立即关闭；**仅完整面板模式显示**（悬浮窗/桌宠窗口过小会被裁剪，CSS 按 `body.float-mode` 门控）。触发时机两条：① 启动——setup 阶段早于 WKWebView 加载、emit 发即被弃，改为前端初始化完成后 `invoke("tray_hint_once")` 领取一次性标志（AppState 的 `tray_hint_pending`，mac 初始 true、领取即清零，非 mac 恒 false）；② 托盘"显示面板"/左键 toggle 唤起隐藏窗口（`show_main`，页面已就绪，直接 emit `tray-hint`）。Windows 两条路径都不触发，前端永不显示。

## 日志系统（main.rs DebugLog）

- `~/.zcode/speed-panel-debug.jsonl`：JSONL 追加写，8MB 轮转为 `.jsonl.1`；轮转旧文件超 7 天在启动时自动删除。
- 事件格式与排查方法见 [key-rules.md](key-rules.md) #6。

## 持久化文件

| 文件 | 内容 |
|---|---|
| `~/.zcode/speed-panel-mode.txt` | JSON：mode/style/full_pos/float_pos/pet_size（旧格式纯文本兼容） |
| `~/.zcode/speed-panel-debug.jsonl` | 调试日志（8MB 轮转 + 7 天清理） |

## CI 与发布

- `.github/workflows/build.yml`：**不随普通推送自动触发**；`v*` 标签 → 自动创建 GitHub Release，Assets 附 Windows 安装版（`_x64-setup.exe`）、免安装版（`_x64-portable.exe`，主程序 exe 直接改名）与 macOS 双架构 dmg（`_x64.dmg` = Intel 10.15+、`_aarch64.dmg` = Apple Silicon 11+，独立包不做 universal，未签名公证见 README 绕过指引）；`workflow_dispatch` → Actions 页手动触发，产物在本次运行的 Artifacts（`windows` 含两个 exe；`macos-x86_64-apple-darwin` / `macos-aarch64-apple-darwin` 各含一个 dmg）。
- **build-macos job**：`runs-on: macos-15`，matrix 双目标（`x86_64-apple-darwin` + `MACOSX_DEPLOYMENT_TARGET=10.15`、`aarch64-apple-darwin` + `11.0`），tauri-action `--bundles dmg`；最低系统版本双重注入——环境变量决定二进制 `LC_BUILD_VERSION`，`--config '{"bundle":{"macOS":{"minimumSystemVersion":"…"}}}'` 决定 Info.plist 的 `LSMinimumSystemVersion`（plist 只认 config，缺省恒 10.13，不读环境变量）。产物名 `zcode-speed-panel_版本_x64.dmg` / `_aarch64.dmg` 由 tauri 默认命名，恰好满足双独立包要求。
- 本地正式版：`npm run tauri build` → Windows `src-tauri/target/release/bundle/nsis/*.exe`；macOS `bundle/dmg/*.dmg`（交叉构建 aarch64 见 README 开发章节）。

## 已知边界情况

- 管道静默调用（实测约半数）实时读数走 ≈ 回退，属预期行为而非 bug（此类调用启动期先显示 20s "…" 提示再切换）。
- 多调用并发（子 agent）时实时优先统计**进行中调用的会话**对应进程（尚无归属时为全进程求和，无进行中调用时退回最近完成调用的会话）。
- 冷启动后系数需 1~2 个达标调用收敛，此前读数可能有偏差。
- 硬崩溃（CLI 进程被杀、assistant 行无人补写 `completed`）最多残留 10 分钟门控（兜底上限）；已归属会话的进程退出会被进程守卫立即判停，未归属的新会话只能等兜底。
- mac 的 CleanParams（burst 禁用/无静态底噪/系数先验 700/延迟落盘宽限 15s/离群拒绝 3 倍）中，先验与宽限已按 2026-09-17 的 6 条 cal 事件真值对账修正（归因正确时 pred/true 完全一致 62.1=62.1），尚未做 Windows 侧同等长度的对账回归；读数异常时先跑 `python scripts/live_vs_true.py` 对账、看 cal 事件 `attr_pid`/`top_pid` 归因再调参（key-rules #10）。
