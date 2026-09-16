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
- **实时速度（10 秒真实值）**：流式输出期间 CLI 会持续向桌面 UI 管道写入渲染增量。面板用操作系统 IO 计数器（GetProcessIoCounters）每 500ms 采样各 CLI 进程的写字节流，取最近 10 秒窗口，扣除每进程自适应心跳噪声底与落盘尖峰（rollout/日志/WAL 文件增量），再除以**自校准系数**折算 token/s。校准系数来自每次调用完成后真实 `output_tokens` ÷ 该调用区间实测字节，滑动中位数持续修正。
- 若 IO 采样不可用，回退为：按今日调用间隔推断仍在生成时显示估算值（≈ 标记），否则归零待机。
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
