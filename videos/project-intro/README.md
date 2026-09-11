# GitHub 项目介绍视频

20 秒、1920×1080（16:9）、30fps 无声视频，使用 HyperFrames 0.8.34 + GSAP 3.14.2。五段子合成可在 Studio 时间轴中预览和编辑。

```bash
cd videos/project-intro
npm run dev
npm run check
npm run render -- --quality high --fps 30 --output renders/qq-maid-bot-intro.mp4
```

需要 Node.js 22+、FFmpeg 与 Chrome；首次运行 CLI 可能下载依赖与渲染浏览器。Logo、GSAP 和字体均已放在 `assets/`，画面不依赖外部图片服务。`index.html` 是合成入口；`compositions/` 是五段源码；`snapshots/contact-sheet.jpg` 是画面总览。MP4、缓存和本地审计报告不入 Git。

## 内容依据

| 时段 | 展示内容 | 仓库依据 |
| --- | --- | --- |
| 0–3s | 名称、原 Logo、自托管多入口定位 | [根 README](../../README.md)；[Logo](../../docs/img/logo.png) |
| 3–8s | 多轮会话、图片理解、受控记忆 | [README 能做什么](../../README.md#能做什么)；[Core](../../qq-maid-core/README.md) |
| 8–13s | 白名单工具、Todo/提醒、RSS/Atom、联网查询 | [Core 工具边界与通知接入](../../qq-maid-core/README.md) |
| 13–17s | QQ 官方、OneBot 11、可选微信文本入口；模型供应商与自动降级 | [平台支持](../../README.md#平台支持)；[LLM](../../qq-maid-llm/README.md) |
| 17–20s | GitHub 地址、MIT、参与共建 | [项目 README](../../README.md)；[LICENSE](../../LICENSE) |

聊天与工具 UI 标为能力示意，文案仅用于介绍功能，并非真实执行回执。未接触真实聊天、配置、账号或模型 API。图片能力取决于入口与所选模型，群聊完整工具循环默认关闭并受场景白名单控制；微信不是个人微信接入。

## 素材与维护

- Logo 使用仓库原文件，不重绘；`.media/` 记录本地素材来源。
- 中文字体为 Noto Sans CJK SC 的字形子集，等宽字体为 DejaVu Sans Mono；许可证位于 `assets/*LICENSE.txt`。新增中文时需重新生成子集或替换为包含所需字形的字体。
- GSAP 文件保留原授权头；来源为 jsDelivr 上的 `gsap@3.14.2/dist/gsap.min.js`。
- 文字入场改编自 registry 的 `staggered-fade-up`，保留组件原始示例；无模糊入场，使用显式 GSAP 起点和有限时间轴，确保跳转时画面一致。
- 不修改应用代码、Web Console 或部署配置。视频修改使用 HyperFrames 检查，不需要 Rust 回归。
