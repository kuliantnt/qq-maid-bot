//! 通用 Agent Tool Turn 的跨工具结果编排测试。
//!
//! 按工具领域拆分可信 outcome 的组合、顺序、状态和用户可见结果测试；
//! 具体工具参数、存储和领域状态机由各工具域测试负责。

use qq_maid_llm::provider::{ToolCallingProtocol, ToolExecutionAttempt};
use serde_json::Value;

use crate::runtime::tools::todo::{TodoItemDraft, TodoStore, TodoTimePrecision};

use super::support::*;

mod outcomes;
mod todo;
mod weather;
mod web_search;
