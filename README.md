# ZCode 速度仪表盘（zcode-speed-panel）

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

一个 Tauri 2 + Rust 的桌面常驻小工具：实时展示 ZCode CLI 的模型输出速度与今日 Token 用量，支持桌宠、迷你仪表盘与速度胶囊三种悬浮窗形态，可最小化到系统托盘。

![预览](app-icon.png)

## 功能

- **悬浮窗模式**（点右上角"⧉ 悬浮窗"收起，置顶、不占任务栏）
  - 三种样式可选（完整面板右上角下拉框）：**桌宠**（200×200，精灵动画随状态切换：生成中跑步、待机站立，头顶气泡显示实时速度，🔄 换宠物）/ **迷你仪表盘**（116×116）/ **速度胶囊**（224×78）
  - 按住悬浮窗任意位置拖动；点右下角 ⤢ 展开回完整面板；托盘菜单也可切换
  - 模式与样式都会记住，下次启动直接恢复
- **近 15 分钟速度曲线**（10 秒一档，对齐墙钟边界，整条曲线随时间连续左移）
- **窗口/托盘**：标题栏实时显示当前速度；托盘左键单击显示/隐藏；右键菜单（显示面板 / 隐藏到托盘 / 悬浮窗切换 / 退出）；点窗口关闭按钮 = 隐藏到托盘；重复启动自动唤起已有窗口
- 状态栏：数据源（usage 数据库）、今日调用次数、会话数、最近活动时间

## 数据源

- **今日用量与平均速度**：只读轮询 ZCode usage 数据库（`~/.zcode/cli/db/db.sqlite` 的 `model_usage` 表，WAL 模式不影响运行中的客户端）。速率定义采用**纯生成时长**：分母 = `completed_at - first_token_at`（排除首 token 等待与排队），分子 = `output_tokens + reasoning_tokens`（思考内容同样是流式输出）。一张表覆盖全部会话（含子 agent）。
- **实时速度（10 秒真实值）**：不依赖落盘数据，直接实测 CLI 进程的写字节流折算而成，原理见下方[《实时速度实测原理》](#实时速度实测原理)。
- 若 IO 采样不可用，回退为：按今日调用间隔推断仍在生成时显示估算值（≈ 标记），否则归零待机。

## 实时速度实测原理

**要解决的问题**：ZCode 只在调用完成时才把 token 数落盘（`model_usage` 表），流式过程中数据库里没有任何增量数据——这是所有"完成后统计"类工具的共同盲区：长回答生成期间，速度只能显示 0 或上一次的旧值。

**关键观测**：CLI 进程在模型流式输出时，会持续把渲染增量写入通往桌面 UI 的管道。对进程写字节速率的实测显示：

| 状态 | 写速率（每 500ms） |
|---|---|
| 流式输出中 | 6 ~ 25 KB，随生成节奏波动 |
| 待机 | 仅 1 ~ 2 KB 心跳 |
| 调用完成瞬间 | 数百 KB 尖峰（落盘写入） |

这个计数器由 Windows 内核维护（`GetProcessIoCounters` 的 `WriteTransferCount`），权威、实时、读取零开销。

**测量循环（每 500ms）**：

1. **发现进程**：Toolhelp32 枚举全部进程 → 过滤 `ZCode.exe` → 读每个进程的完整命令行（ProcessBasicInformation → PEB → ProcessParameters → CommandLine），只保留含 `zcode.cjs` 的 CLI 子进程（每 30s 刷新，排除桌面壳/渲染进程）
2. **采样**：对每个 CLI 进程读取 `WriteTransferCount`（进程启动以来累计写字节），存入每进程约 130s 的环形缓冲
3. **字节速率** =（当前累计值 − 10 秒前累计值）÷ 10
4. **扣心跳噪声**：每进程取自身历史最小增量的 2 倍作为底噪扣除（自适应，各进程底噪不同）
5. **扣落盘尖峰**：同步监测 rollout/日志/WAL 文件大小增量，调用完成瞬间的写盘字节不计入输出
6. **字节 → token**：除以**自校准系数**。每次调用完成后，用它的真实 `output_tokens` ÷ 该调用流式区间实测字节得到一次校准样本，取最近 5 次的中位数作为当前系数（实测约 1600 B/token，随内容类型自动漂移修正）

**只看当前会话**：每次调用完成时，取流式区间内写字节最多的进程，标记为该会话所属的 CLI 进程；实时速度只统计"当前会话"（最近完成调用的会话）对应的那一个进程。后台 bot、其他窗口的流量被天然隔离，不会造成"已经停止了还显示生成中"。

**实测效果**（标题栏轨迹，每 1.2s 采样）：

```
19:56:54  ⏸ 0.0 t/s     ← 无任务
19:57:04  ▶ 16.4 t/s    ← 流式输出开始的瞬间
19:57:10  ▶ 44.0 t/s    ← 全速生成
19:57:21  ▶ 9.5 t/s     ← 末段节奏放缓（真实反映）
19:57:35  ⏸ 0.0 t/s     ← 停止后 10 秒窗口滑过，如实归零
```

**精度说明**：字节 → token 的换算是统计近似的（受 UI 帧内容影响约 ±20%），但字节速率本身是 500ms 粒度的真实观测。精确 token 计数仍由每次调用完成时的落盘记录提供，用于今日总量与均值——两者互为补充。

- 今日 Token 总量与 ZCode 官方统计同口径 = `input + output + reasoning + cache_creation`；**缓存命中（cache_read）是提示复用、不计入总量**，明细中单独展示。按事件完成时间归属"今日"，跨天自动重置。

> 隐私：所有数据仅从本地文件与进程读取，不上传任何内容。

## 开发与构建

环境要求：Node.js ≥ 20、Rust stable（Windows 下需 MSVC 工具链）、WebView2 运行时（Win11 自带）。

```bash
npm install
npm run tauri dev      # 开发调试（debug 版连接 vite dev server）
npm run tauri build    # 正式版（内嵌前端 + NSIS 安装包）
```

产物位置：

- 可执行文件：`src-tauri/target/release/zcode-speed-panel.exe`
- 安装包：`src-tauri/target/release/bundle/nsis/*.exe`

调试工具（不走 UI，直接打印引擎对真实数据的计算结果，含 IO 实测可用性）：

```bash
cd src-tauri && cargo run --example dump
```

单元测试：

```bash
cd src-tauri && cargo test
```

## 浏览器预览

`npm run dev` 后直接在浏览器打开 <http://localhost:1420>，页面会以模拟数据运行（检测到非 Tauri 环境自动进入 mock 模式），便于调样式。

## 技术栈

Tauri 2（Rust 后端：usage 数据库轮询 + 进程 IO 实测 + 托盘）、原生 Canvas 绘制仪表盘与桌宠（无图表库依赖）、Vite + TypeScript。

## 致谢

- [zcode-tps-monitor](https://github.com/shy3130/zcode-tps-monitor)（MIT）：本项目速率定义借鉴其纯生成时长口径（first_token_at 起点、思考 token 计入分子）
- [dsh-desk](https://github.com/Renakoni/dsh-desk)（MIT）：桌宠采用其内置的 Codex Pet 宠物包（月薪喵、Maid-DeepSeek-Whale）

详见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

## 协议

[MIT](LICENSE)
