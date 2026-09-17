# 关键规则与踩坑详情

改代码前必读。每条都是本项目真实发生过的事故，含症状与根因；违反会导致难以察觉的功能失效。

## 1. `$()` 是 getElementById，找不到直接 throw

`src/main.ts` 的 `$()` 按 `document.getElementById` 查找，元素不存在时抛 `missing #xxx`。
**模块加载中途抛错 = 前端整体瘫痪且无提示**：metrics/mode 事件监听全部未注册，表现为数值全 0、模式切换失效；而挂在崩溃点之前的监听仍然生效，症状呈现"半坏"假象。
- 事故案例：顶栏是 `<header>` 标签没有 id，`$("header")` 直接炸（已修：`#app-header`）。
- 规则：HTML 新增节点必须带正确 id；改动 main.ts 后必跑 `npx tsc --noEmit` + 浏览器 mock 模式（`npm run dev` 后开 localhost:1420）验证渲染。

## 2. Tauri 2 权限白名单：未放行的窗口 API 一律被拒

前端调用窗口核心 API（minimize / toggleMaximize / startDragging…）必须在 `src-tauri/capabilities/default.json` 的 `permissions` 里显式放行（如 `core:window:allow-minimize`），否则调用被拒绝。
- 事故案例：— / ▢ 按钮无反应——权限缺失，且 `.catch(() => {})` 把报错静默吞掉（已修：补权限）。
- 规则：新增窗口 API 调用时同步加权限；**不要用空 catch 掩盖失败**，至少 `console.warn`。dev 版可 F12 看权限报错。自定义命令（`#[tauri::command]` + `generate_handler!`）不受此限制。

## 3. 实时速度的一致性校准：校准与显示必须同一条清洗流

字节→token 系数的分子必须来自与显示完全相同的清洗流（`build_rows` 产物在调用区间的积分），系统性扣除（噪声底/落盘镜像/突发剔除）才能被系数抵消。
- 事故案例：校准用未清洗总字节（~900-1400 B/token）、显示用清洗流（真值 ~350-550 B/token），系数被抬高 2~3 倍，实时读数只有真值的 1/5（~10 t/s vs 40-50）。
- 规则：改 liveio 采样/清洗逻辑时，`measure()` 内校准积分与显示幅度必须同源；合成端到端测试（`synthetic_call_converges_to_true_tps` 等）是守护，必须保持通过。

## 4. 逐拍清洗禁止钳非负（负拍对消）

落盘 flush 与 IO 计数存在错位：单拍可能出现"文件涨 60KB 但进程只写 20KB"。该拍必须记为负值，靠区间积分（`integrate`）与前后正拍对消；只在窗口汇总处 `max(0, ·)`。
- 事故案例：旧版逐拍 `max(0, delta-落盘)`，错位字节永久丢失，约一半调用读数塌缩到 ~130 B/token。
- 关联：自适应噪声底必须封顶（`FLOOR_CAP_BYTES=2KB`），否则持续流式期间分位数被流式增量毒化，扣掉自身输出。

## 5. 校准样本准入：静默调用会污染系数

相当一部分调用是"管道静默"——生成期间 UI 管道几乎无增量字节，全部在完成瞬间刷出。这类调用的管道积分 ÷ token 可低至 ~7 B/token，入样会把中位数系数拉低、实时读数虚高。
- 规则：样本必须经 `cal_sample` 准入（eff ≥ 300 token、clean ≥ raw 的 20%、B/token 在 [100, 6000] 且未触钳位），拒绝的整条丢弃（日志 `skipped=true`）。系数队列预置 600 先验，冷启动单个异常样本无法独占中位数。

## 6. 调试日志是排查的第一手数据

`~/.zcode/speed-panel-debug.jsonl`（JSONL 追加，8MB 轮转保留一代 `.jsonl.1`，轮转旧文件超 7 天启动时自动清理）记录三类事件：
- `tick`：显示值 `tps`、来源 `src`（io/window/idle）、启动期 `start`、清洗管道字节率 `pipe`、生效系数 `bpt`、曲线尾桶；
- `call`：调用完成真值（`eff`/`gen_ms`/`true_tps`）；
- `cal`：校准对账（`true_tps` vs `pred_tps`、`raw_kb`/`clean_kb`、`bpt_sample`/`bpt_now`、`skipped`）。

实时准确性评估口径：`pred_tps / true_tps` → 1.00 为准。用 `python scripts/live_vs_true.py` 一键对账（≥300 token 且入校准的调用为达标样本）。诊断实时读数问题先看这里，不要靠猜。

## 7. examples 与 src 的编译耦合

`src-tauri/examples/{dump,verify}.rs` 通过 `#[path]` 直接 include `metrics.rs`/`liveio.rs` 编译。改这两个模块的公开 API（结构体字段、函数签名）时，examples 也必须同步更新，否则 `cargo test`/`cargo build --examples` 失败。

## 8. WebView2 原生 `<select>` 弹层跟随系统主题，深色 UI 里不可读

原生 select 的**下拉弹层**由 WebView2 按系统主题渲染：系统浅色时弹层白底，option 又继承了页面里的灰字样式，深色界面下几乎看不见；实测 `:root { color-scheme: dark }` 与 option 显式着色在 WebView2 弹层里均不生效。
- 事故案例：悬浮窗样式下拉"看不见字"（已修：整个替换为自绘 `.dropdown`/`.dropdown-list`，与右键菜单同风格深色弹层）。
- 规则：本项目 UI 需要下拉一律自绘，不再新增原生 `<select>`；新增顶栏交互组件时，同步把它加入 enableDrag 拖动与双击最大化的排除选择器（`button, select, input, .dropdown`），否则会误触窗口拖动/最大化。

## 9. 启停门控必须用 message 行的 `completed` 字段，不能用 model_usage 完成行

调用启停判定以 usage 库 message 表 assistant 消息行为准：**行在调用开始瞬间提交（≤200ms），行内 data 的 `time.completed` 在结束瞬间补写（取消/出错也会补）**。`model_usage` 行只记 `status='completed'`——cancelled/error 的调用（实测库中 97+119 条）**永远没有完成行**，用完成行判停会卡"生成中"直到 10 分钟兜底；且它不区分会话，新开对话首个调用要等首个完成行落盘才可见（长调用可达数分钟）。
- 事故案例：旧口径"最新 assistant 创建时间 > 最新完成调用的 completed_at 且限最新完成调用的会话"——用户取消生成后面板持续显示"生成中 + 估算值"最长 10 分钟；新会话开聊全程无反应。
- 查询约束：message 表**没有 time_created 单列索引**（全局 `ORDER BY time_created DESC` 实测 ~200ms/次，700ms 轮询不可承受）——必须先取 `session.time_updated` 倒序前几个会话，再走 `(session_id, time_created)` 复合索引按会话查最新 assistant 行。
- 守护：`inflight_from_rows`（metrics.rs）为门控纯函数单测（僵尸行/多会话/超龄）；`awaiting_hint`（liveio.rs）守护启动期提示窗口。改门控相关代码时这两个测试必须保持通过。

## 10. mac 平台差异（照搬 Windows 参数会静默失效）

实时测速的平台原语在 `liveio::platform`（win/mac/stub 三份 cfg），清洗/校准参数由 `CleanParams` 平台参数化。以下差异都是实测撞出来的，跨平台改 liveio 前必读：

- **mac 的 burst 即信号，必须禁用 BURST_TICK_BYTES**：Windows 上单拍 >100KB 是请求体上传（应整拍剔除）；mac 上流式本身就是单拍突发形态（实测单拍 +225KB~1.5MB 是常态，0,0,0,+大块 交替），沿用 100KB 阈值会把**全部**流式信号当突发丢掉，实时读数恒 0。mac 侧取 `u64::MAX` 禁用。
- **files 扣除在 mac 是方向性反噬**：Windows 的落盘扣除（tracked 文件增量从写字节中减去）在 mac 必须关闭——CLI 会清理轮转旧 rollout，实测 120s 探针里 rollout 目录 du **净变化为负**，负的文件增量会把清洗流反向抬高（而不是扣除）。mac 的 `tracked_files_total` 恒 0。
- **mac 进程识别不能用 proc_pidpath**：CLI 进程由 Electron Helper fork 而来，`proc_pidpath` 返回的是 `.../ZCode Helper`（与其他 Helper 进程同一路径，无法区分）；`ps` 显示的 "zcode-cli" 是 p_comm。正确口径：`KERN_PROCARGS2` 打包区里扫描独立的 NUL 结尾字符串精确匹配 `zcode-cli`（注意 argv[0] 之后有**对齐 NUL 填充**，不能按 nargs 连续解析，否则读到一堆空串）。曾经按 basename 匹配实现过一版，dump 冒烟 `live可用=false`。
- **FFI 偏移错位用 offset_of! 编译期断言防**：`proc_pid_rusage` 的 `rusage_info_v4` 结构镜像必须逐字段对照 SDK `sys/resource.h`，且用 `offset_of!` 断言 `ri_proc_start_abstime`=80、`ri_diskio_byteswritten`=152（新内核布局在 start_abstime 后多了 `ri_proc_exit_abstime`，老布局记忆是 144——就是这个坑）。断言不过必须修结构排布，**禁止删断言**；另 `#[link(name = "proc")]`（库文件是 libproc.dylib，链接名不带 lib 前缀，写 "libproc" 会 `ld: library not found for -llibproc`）。
- **Dock/Cmd+Tab 与退出的上游限制**：Tauri/macOS 上无边框窗口应用保留 Dock 图标，`hide()` 也无法把 Accessory 应用完全"藏起来"；本项目采用 **Accessory 模式**（`set_activation_policy`，setup 内尽早调用）+ 自定义菜单拦截 `Cmd+Q`（菜单不含任何 `PredefinedMenuItem::quit`）+ `RunEvent::ExitRequested { code: None }` 兜底 `prevent_exit`。三条防线合起来才保证"真退出只有托盘退出与悬浮窗右键退出两条路"——只做其中一两条，用户仍可能从系统菜单/快捷键把应用退掉，之后菜单栏入口消失、体验等于"应用丢了"。
