//! qq-maid-llm 程序入口。仅负责启动异步运行时并委托 app::run 完成初始化。

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    qq_maid_llm::app::run().await
}
