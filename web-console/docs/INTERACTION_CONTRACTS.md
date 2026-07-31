# Interaction Contracts

这份文档定义配置中心和其他高风险操作的交互协议。视觉组件必须服从这些状态，不得用一个绿色 Toast 代替真实的保存结果。

## 1. 配置编辑器状态

每个配置域独立维护状态。当前配置域包括 `runtime`、`secrets` 和 `agent`。

```text
loading -> clean -> dirty -> validating -> saving -> saved
                         \-> conflict
                         \-> error
任何状态 -> session_expired
saved + pending_restart -> pending_restart
```

状态含义：

| 状态 | 用户看到的内容 | 允许的操作 |
|---|---|---|
| loading | 骨架或“正在加载配置” | 不能编辑 |
| clean | 当前值与服务端 snapshot 相同 | 编辑、刷新 |
| dirty | “有未保存修改”和变更数量 | 保存、撤销、继续编辑 |
| validating | “正在校验…” | 锁定当前域，不能重复提交 |
| saving | “保存中…” | 锁定当前域，不能离开并丢弃草稿 |
| saved | 服务端返回的新 revision | 继续编辑、刷新 |
| pending_restart | “X 项重启后生效” | 保存其他域、请求重启 |
| conflict | 本地草稿和服务器版本不同 | 对比、采用服务器、重新提交 |
| error | 安全错误摘要和重试动作 | 修改、重试、重新加载 |
| session_expired | 会话已失效 | 重新登录；草稿默认保留在内存 |

### Dirty 判定

- 每个域保存 `serverSnapshot`、`serverRevision` 和 `draft`。
- 字段值与该域加载时的 baseline 比较，不能用输入框是否非空判断 dirty。
- 空字符串对未配置的可选普通字段仍然表示“未修改”；用户要移除已有普通值时使用明确的“移除”动作。
- 密钥输入框非空表示待 replace，空白表示不修改，不能因此推断原密钥为空。
- 刷新、切换页面或退出前，如果域 dirty，必须显示“保留修改 / 丢弃修改 / 取消”。不能静默覆盖草稿。

## 2. 普通配置保存

普通配置使用 `PATCH /api/v1/console/configuration/runtime`。

交互顺序：

1. 用户修改字段，域进入 dirty，SaveBar 显示变更数量。
2. 客户端先做字段类型、必填、范围和互相依赖的校验。
3. 只收集实际变化，生成 `set` 或用户明确触发的 `remove`，不提交无变化字段。
4. 使用加载时的 `expected_revision` 提交。
5. 成功后以服务端返回的 snapshot 和 revision 重建表单，清除 draft dirty 状态。
6. 如果字段 `applyMode` 是 restart，显示“已保存，X 项需重启后生效”，不能显示“已运行”。

按钮和反馈文案：

- `保存普通配置`
- `保存中…`
- `没有需要保存的普通配置`
- `已保存，X 项需重启后生效`
- `配置已保存并立即生效`

保存期间只锁定普通配置域和其 SaveBar，不能无差别锁定 Markdown、状态刷新或其他未关联页面。

## 3. Agent 配置保存

Agent 使用 `PATCH /api/v1/console/configuration/agent`，所有变更共享 agent revision。

- Provider、知识检索、联网搜索、模型路线和 scene/tool whitelist 分组展示。
- 默认保存整个 Agent 草稿，但只生成有变化的结构化 action。
- 私聊和群聊工具白名单可以独立保存；独立保存成功后只更新该 scene 的 baseline。
- Provider 连接测试不是保存动作；它读取当前解析环境并独立显示结果。
- Agent 保存成功后，如果 agent snapshot 标记 pending restart，显示待重启数量和受影响分组。

## 4. 密钥添加、替换和清除

密钥是高风险字段，服务端永远不返回原文。新增和替换使用同一个输入组件，但文案根据 `configured` 状态变化：

| 当前状态 | 输入提示 | 空白行为 |
|---|---|---|
| 未配置 | `尚未配置，输入后添加` | 不提交 |
| 已配置 | `已配置；留空表示不修改` | 不提交 |

### 添加或替换

1. 输入框默认 `type=password`，显示/隐藏只影响当前内存值。
2. 输入框必须标记 dirty，但不能把值写入 localStorage、URL、日志、data 属性或错误消息。
3. 保存时发送 `replace` 和该字段的 `expected_revision`。
4. 保存成功后立即清空输入框，恢复 password 类型，显示“密钥已保存，原文不会再次显示”。
5. 页面刷新后只显示“已配置”，绝不尝试恢复原文。

### 清除

1. 清除是显式 checkbox 或按钮，不由空输入触发。
2. 用户触发后显示二次确认，明确指出关联功能可能停止工作。
3. 确认后才生成 `clear`，并携带该字段的 expected revision。
4. `replace` 和 `clear` 对同一个 key 互斥；如果用户重新输入，清除选择必须自动取消。
5. 成功后显示“密钥已清除”，并重新加载配置状态。

推荐文案：

- `空白不会修改密钥`
- `显式清除密钥`
- `确认清除这个密钥吗？清除后依赖它的功能可能无法使用。`
- `密钥已保存，原文不会再次显示`
- `密钥已清除`

## 5. Revision 冲突

收到 HTTP 409 或 `config_conflict` 时：

1. 停止自动重试，不能用新 revision 自动覆盖服务器值。
2. 保留当前域 draft 和用户输入。
3. 重新 GET configuration，取得服务器当前 snapshot。
4. 展示冲突字段的本地草稿、服务器当前值和来源。secret 冲突只显示“服务器已配置/未配置”，不能显示值。
5. 提供三个动作：
   - `采用服务器值`：丢弃冲突字段本地草稿。
   - `保留我的修改并重新提交`：以最新服务器 revision 重新构造变更，需用户再次确认。
   - `逐项选择`：普通字段逐项选择；secret 只能选择保留本地 replace 或采用服务器状态。
6. 解决前禁止显示“保存成功”。

反馈文案：

`配置已被其他操作修改，未覆盖服务器版本。请比较本地修改和服务器当前值。`

## 6. 校验、连接测试和重启

### 配置校验

`POST /configuration/validate` 只执行与正式启动一致的本地预检：

- 不是保存。
- 不执行公网网络请求。
- 不改变配置 revision。
- 失败时定位到字段或配置组。

文案：`配置校验通过，未执行外部网络请求` 或 `配置未通过启动预检，未保存任何变更`。

### Provider 连接测试

`POST /configuration/test-connection` 是独立动作：

- 只测试受控 HTTPS `/models` 探测。
- 不修改配置。
- 不代表聊天请求、模型生成或所有凭据均可用。
- OpenCode 结果只表示官方匿名目录可达。

测试期间按钮显示 `测试中…`，结果同时显示分类和安全说明。

### 服务重启

重启前使用自定义 ConfirmDialog，而不是 `window.confirm`。确认内容必须写明：

`重启会使服务短暂离线，当前未保存修改不会自动保存。继续吗？`

提交成功只能显示：

`重启请求已提交，服务会短暂离线`

不能显示“重启成功”。之后进入 `restarting` 状态，暂停普通保存，轮询或按刷新操作确认 `/status` 恢复；恢复后显示新 runtime 状态。

## 7. 可复用交互组件

| 组件 | 责任 |
|---|---|
| `SaveBar` | dirty 数量、域级保存按钮、保存中和保存结果 |
| `DirtyIndicator` | 显示“有未保存修改”，不只使用颜色 |
| `ConfigField` | label、输入、来源、apply mode、valid、pending restart |
| `SecretField` | password 输入、显示/隐藏、configured、clear、replace/clear 互斥 |
| `ConflictPanel` | 本地 draft、服务器 snapshot、字段选择和重新提交 |
| `ConfirmDialog` | 清除 secret、重启等危险操作的二次确认和焦点回收 |
| `InlineStatus` | 页面内 loading、success、warning、error |
| `Toast` | 全局短反馈，不承担冲突合并或详细错误解释 |
| `SessionExpiredOverlay` | 会话过期提示、重新登录、保留内存草稿 |

这些组件只接受 typed state 和 callbacks，不直接访问 API。组件状态必须可被键盘操作，并使用 `aria-live`、`aria-describedby` 和明确的 focus return。

## 8. 禁止行为

- 不把“请求已发送”写成“服务已重启”。
- 不把空 secret 当成清除命令。
- 不回填、缓存、记录或回显 secret 原文。
- 不在 revision 冲突时自动重试或静默覆盖。
- 不用 Toast 隐藏字段级错误。
- 不在 dirty 草稿上执行无提示刷新。
- 不用颜色单独表示保存、冲突或错误状态。
