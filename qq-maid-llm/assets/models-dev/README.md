# Models.dev fixed upstream input

本目录保存当前内置 Catalog 快照使用的固定上游输入，供开发期转换器离线复现。

- `upstream.json`：从 `https://models.dev/api.json` 提取 `openai`、`deepseek`、
  `google`、`zhipuai` 四个 Provider 后原样保留的 JSON；不手工规范化或合并。
- `manifest.json`：与本次抓取对应的真实来源 URL、上游 commit（Models.dev `dev`
  分支 HEAD）和抓取时间。

重新生成内置快照（必须在仓库根目录执行，且不得在 Release 构建中调用）：

```bash
cargo run -p qq-maid-llm --bin modelsdev-converter -- \
  qq-maid-llm/assets/models-dev/upstream.json \
  qq-maid-llm/assets/models-dev/manifest.json \
  qq-maid-llm/assets/model-catalog.json
```

转换器只提取白名单字段，输出稳定排序，并把本输入文件的 SHA-256 写入
`source_hash`。`upstream.json` 不得手工修改；需要跟随上游更新时，应重新抓取并同时
更新 `manifest.json` 的真实来源信息，然后运行上述命令。

来源与许可记录见仓库根目录 `THIRD_PARTY-NOTICES.md` 和 `LICENSES/MIT.txt`。
