# 第三方组件与素材声明（Third-Party Notices）

本项目包含或借鉴了以下开源项目的成果，感谢原作者。

## 1. 桌宠宠物包（pet packs）

- 来源项目：[dsh-desk](https://github.com/Renakoni/dsh-desk)
- 作者：Renakoni
- 协议：MIT License，Copyright (c) 2026 Renakoni
- 使用内容：`public/pets/yuexinmiao/` 与 `public/pets/maid-deepseek-whale/`
  中的精灵图（spritesheet.webp）与 pet.json，以及 Codex Pet 宠物包格式的
  精灵图布局约定（1536×8 列、9 行动画映射、160ms/帧）。

## 2. 速率定义参考

- 来源项目：[zcode-tps-monitor](https://github.com/shy3130/zcode-tps-monitor)
- 作者：shy3130
- 协议：MIT License，Copyright (c) 2026 shy3130
- 使用内容：本项目借鉴其"纯生成时长"速率口径（以 `first_token_at` 为
  分母起点、思考 token 计入分子），数据源同为 ZCode usage 数据库的
  `model_usage` 表（只读访问方式参考其文档）。本项目未复制其源代码。
