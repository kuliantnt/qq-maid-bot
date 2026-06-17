# llm/ — LLM 运行配置目录

本目录是服务器运行目录示例。真实 `.env`、成员映射、世界观和提示词都属于本地私有配置；
仓库只保留 `.example` 模板，用于说明字段含义。生产部署可以通过 `.env` 把路径指向外部私有配置仓库或本机私有目录。

## 目录结构

```
llm/
├── .env                             # 环境变量（QQ 机器人、LLM 供应商、服务端口）
├── README.md                        # 本文件
└── config/
    ├── world.example.md             # 可提交的 WORLD_FILE 模板
    ├── world.md                     # 可选世界观文件，路径由 WORLD_FILE 指定，不提交
    ├── member_id_mapping.example.json
    ├── member_id_mapping.json       # 本地私有成员编号映射，不提交
    └── prompts/
        ├── *.example.md             # 可提交的通用模板
        ├── maid_system.md           # 本地私有系统提示词，不提交
        ├── mode_rules.md            # 本地私有模式规则，不提交
        └── session_context.md       # 本地私有会话上下文规则，不提交
```

## 各文件说明

### `.env`

全局环境变量。控制 QQ Bot SDK 参数、LLM 供应商（OpenAI / DeepSeek）、LLM 服务监听地址、超时和外部配置路径等。
**注意：包含密钥，不要提交到公开仓库。**

和私有配置仓库相关的常用路径变量：

- `PROMPT_DIR`：包含 `maid_system.md`、`mode_rules.md`、`session_context.md` 的目录。
- `WORLD_FILE`：可选世界观文件；留空表示按通用助手运行。
- `MEMBER_ID_MAPPING_FILE`：成员编号映射 JSON 文件。
- `APP_DB_FILE`：运行数据库文件，应放在不进 Git 的数据目录。

### `config/member_id_mapping.json`

成员编号映射。键为成员编号（字符串），值为名称和简介。JSON 格式不支持注释，字段含义：

- `name` — 成员名称
- `profile` — 一句话简介

真实成员映射可能包含个人信息或私人设定，应保留在外部私有路径或本地未跟踪文件中。公开仓库只提交
`member_id_mapping.example.json`。文件不存在时按空映射处理；JSON 语法错误会启动失败。

### `config/prompts/maid_system.md`

**核心系统提示词**。定义助手职责、默认语气、QQ 群聊规则、现实问题规则和安全规则。
修改此文件会直接影响机器人的回复风格。真实提示词不提交，公开仓库只提交 `.example.md`。

### `config/world.md`

可选世界观或角色设定提示词。正式入口是 `.env` 中的 `WORLD_FILE`，不再要求把世界观固定写入
`PROMPT_DIR/innerworld_lore.md`。未配置 `WORLD_FILE` 时按通用助手运行；一旦配置，文件必须存在、可读且非空。

开源前如果曾提交过真实世界观，需要额外清理 Git 历史；单纯从当前 HEAD 删除不能移除历史记录。

### `config/prompts/mode_rules.md`

根据用户消息内容自动判断应进入的模式：

1. 日常聊天模式
2. 整理归档模式
3. 方案建议模式
4. 低打扰支持模式
5. 现实问题模式

### `config/prompts/session_context.md`

多轮对话的上下文处理规则：

- 前台成员可能切换或多人同时在场
- 如何判断当前说话者
- 短句（"对啊""继续""给 codex"）优先理解为补充而非新话题
- slash 指令已由程序处理，不要假装执行

## 联动关系

```
.env (供应商/密钥)
  └→ Rust LLM Server (127.0.0.1:8787)
       └→ /v1/respond 接口
            └→ 组装 system prompt:
                 maid_system.md + mode_rules.md + session_context.md
                 + WORLD_FILE（可选）
                 + member_id_mapping.json (注入为成员信息)
```

运行前可按 `.example` 模板复制为无 `.example` 后缀的本地文件，也可以直接把 `.env` 中的路径变量指向外部私有配置仓库。Secret、数据库、日志和聊天记录不应进入任何 Git 仓库；真实 prompt、世界观和成员映射只应放在私有配置仓库或本地私有目录，不进入公开仓库。
