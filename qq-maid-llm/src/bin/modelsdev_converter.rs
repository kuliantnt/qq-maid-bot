//! 开发期固定 Models.dev → Catalog 快照转换器。
//!
//! 用法（仓库根目录）：
//! ```bash
//! cargo run -p qq-maid-llm --bin modelsdev-converter -- \
//!   qq-maid-llm/assets/models-dev/upstream.json \
//!   qq-maid-llm/assets/models-dev/manifest.json \
//!   qq-maid-llm/assets/model-catalog.json
//! ```
//!
//! 生成过程完全离线、只读本地上游输入；build/release 不调用本工具。

use std::{env, fs, process};

use qq_maid_llm::model_catalog::converter::{ModelsDevSourceManifest, convert_snapshot};

fn main() {
    if let Err(message) = run() {
        eprintln!("modelsdev-converter 失败：{message}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "用法：modelsdev-converter <upstream.json> <manifest.json> <model-catalog.json>"
                .to_owned(),
        );
    }
    let input = fs::read(&args[0]).map_err(|error| format!("读取 {} 失败：{error}", args[0]))?;
    let manifest_bytes =
        fs::read(&args[1]).map_err(|error| format!("读取 {} 失败：{error}", args[1]))?;
    let manifest: ModelsDevSourceManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("解析 {} 失败：{error}", args[1]))?;
    let output = convert_snapshot(&input, &manifest)
        .map_err(|error| format!("转换失败：{}", error.message))?;
    fs::write(&args[2], output).map_err(|error| format!("写入 {} 失败：{error}", args[2]))
}
