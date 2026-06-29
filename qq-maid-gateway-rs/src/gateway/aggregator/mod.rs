//! Gateway 入站消息聚合器。
//!
//! 聚合发生在 Dispatcher 之前：等待用户短暂停止输入时不占用 scope worker、
//! worker slot 或 LLM permit。命令和 pending 分类通过 Core 的轻量接口完成，
//! Gateway 只处理平台字段、/ping 本地命令和附件等自身边界。

mod actor;
mod batch;
mod handle;
mod types;

#[cfg(test)]
pub(super) use handle::AggregationDispatcher;
pub(super) use handle::{MessageAggregator, MessageAggregatorHandle};

#[cfg(test)]
mod tests;
