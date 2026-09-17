# AGENTS.md

ZCode 速度仪表盘：Tauri 2 + Rust 桌面工具（Windows 为主），实时实测 ZCode CLI 的流式输出速度并统计今日 token 用量。前端为无框架 TypeScript + Canvas（Vite），后端 Rust 通过 `~/.zcode/cli/db/db.sqlite`（只读）与进程 IO 计数取数。

## 命令

```bash
npx tsc --noEmit                    # 前端类型检查（改 src/*.ts 后必跑）
npm run build                       # tsc + vite build
cd src-tauri && cargo test          # Rust 单元测试（14 个，含合成端到端）
cd src-tauri && cargo check         # 后端编译检查
npm run tauri dev                   # 开发运行（用户常驻一个 dev 实例，改码会热重启它）
npm run tauri build                 # 正式版 + NSIS 安装包
python scripts/live_vs_true.py      # 对账：实时读数 vs 落盘真值
```

## 架构概要

```
src-tauri/src/metrics.rs   数据层：usage 库轮询 + 当日聚合（Engine/Aggregator，纯函数可测）
src-tauri/src/liveio.rs    实时测速：Windows 进程写字节流采样 + 跨平台纯函数（清洗/积分/校准）
src-tauri/src/main.rs      应用层：轮询线程、窗口模式/位置持久化、托盘、DebugLog
src-tauri/examples/        dump/verify 调试工具（#[path] include src，改公开 API 须同步）
src-tauri/capabilities/    Tauri 前端权限白名单（窗口 API 必须在此放行）
src/                       前端：main.ts 装配 / gauges.ts 绘制 / pet.ts 桌宠 / mock.ts 预览
public/pets/               宠物包资源；scripts/*.py 调试日志分析；.github/workflows/ CI
```

数据流：poller 线程每 ~700ms 一拍 → `Engine.poll`（SQLite 增量摄取）→ `Engine.snapshot`（当日聚合 + 90 桶曲线）→ `LiveIo.measure`（进程 IO 清洗/门控/一致性校准 → 覆写实时值）→ DebugLog（JSONL）→ `emit("metrics")` → 前端渲染。

## 规则与踩坑（全在 docs，AGENTS 不留副本）

全部关键规则与踩坑案例集中在 **[docs/key-rules.md](docs/key-rules.md)**，改代码前必读。速览：`$()` 启动崩溃、Tauri 权限白名单、一致性校准口径、负拍对消、校准样本准入、调试日志排查法、examples 编译耦合、原生 select 弹层不可读、启停门控用 message 行 completed 字段（禁用 model_usage 完成行）——共 9 条，每条含事故案例与守护措施。

## 约定

- 提交信息用中文，首行概括根因/行为。
- 无系统标题栏：顶栏自绘（`#app-header` + `data-tauri-drag-region`，左侧为 `app-icon.png` 应用图标）；点 ✕ = 收起为悬浮窗，退出走托盘/右键菜单。
- UI 下拉一律自绘（`.dropdown`），禁用原生 `<select>`——WebView2 弹层跟随系统浅色主题，深色界面里看不见字（key-rules #8）；顶栏新增交互组件须加入拖动/双击排除选择器。
- 仪表配色：速度表分档色定义在 `src/gauges.ts` 顶部 `SPEED_TIERS`（0–30 绿 / 30–60 黄 / 60+ 红，整弧换色不分段，背景轨道恒灰），主表、迷你仪表、"上轮"角标小表（`BadgeGauge`）与胶囊/桌宠的上轮读数共用（`speedColor()` 统一取色）；浮动窗口尺寸改动须同步 `main.rs` 的 `FLOAT_*_SIZE`、`docs/features.md` 与 README。
- CI 不随推送自动触发（省机时）：出包走 `v*` 标签（自动发 Release）或 Actions 页手动 Run workflow（Artifacts）；改动 workflow 触发逻辑须同步 README 与 `docs/features.md`。
- 完整面板与悬浮窗位置各自独立记忆（`~/.zcode/speed-panel-mode.txt`）；悬浮窗尺寸用逻辑像素，物理换算走 `scale_factor()`，多屏定位必须 `clamp_to_screen`。

## 文档索引（按需阅读）

| 文档 | 内容 |
|---|---|
| [docs/key-rules.md](docs/key-rules.md) | 关键规则与踩坑详情（事故案例、症状、守护测试） |
| [docs/features.md](docs/features.md) | 功能详情：统计口径、实时速度回退链、悬浮窗/桌宠行为、图表参数、日志、CI |
| [README.md](README.md) | 面向用户的功能说明与实时测速原理 |
| [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) | 第三方素材与许可 |

## 文档维护（每次改码必做）

**任何程序修改（行为、功能、UI、口径、参数、文件格式）必须在同一提交内同步更新文档**：

- 改功能行为/参数/交互 → 更新 `docs/features.md`；改架构/规则/坑 → 更新本文件与 `docs/key-rules.md`；
- 用户可见的变化 → 更新 `README.md`；
- 新增调试/分析脚本 → 在本文件命令区与 `docs/features.md` 登记。

文档与代码不一致视为改动未完成。提交前自查：`git diff` 里的每处行为变化，是否都有对应文档改动。
