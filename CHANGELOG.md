# Changelog

## [Unreleased] — fix/pr5-remediation

### Added
- Web 控制台路由安全头中间件（X-Content-Type-Options、X-Frame-Options、CSP）
- `scripts/validate-runtime.sh` — runtime 目录完整性校验脚本
- `scripts/botctl.sh` — 统一启停控制脚本（start/stop/restart/status/logs/health/console）
- 群消息 `group_message_mode` 配置项，支持 `off` / `command` / `mention` / `active`
- OpenAI 兼容 GLM provider 支持
- `qq-maid-gateway-rs` 推送到 `qq-maid-llm` 内部 `/internal/push` 端点，支持群 @ 消息透传

### Changed
- `deploy.sh` 增加构建产物校验和缺失检测
- `Makefile install` target 正确拷贝 release 二进制和控制脚本到 `runtime/`
- `scripts/llmctl.sh` 增加 `LINES` 日志行数配置支持
- `scripts/llmctl.sh` 增加 `console` 子命令
- `runtime/.env.example` 更新配置项
- `qq-maid-llm` Web 控制台功能：配置开关、CORS 管理、安全头、Markdown 渲染接口

### Fixed
- 移除 unused import `Body` 警告
- `cargo fmt` 格式化修复

### Config
- 运行时 `runtime/config/.env`：`QQ_MAID_ENABLE_GROUP_MESSAGES=true` → `QQ_MAID_GROUP_MESSAGE_MODE=mention`
  - `Off` 模式不注册群事件 intent，QQ 不会推送 @消息；`mention` 模式支持 @机器人 + 斜杠命令 + 回复机器人消息
