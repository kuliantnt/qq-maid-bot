//! Gateway 内部平台抽象边界。
//!
//! `model` 只定义平台无关入站结构；各平台 adapter 负责把原始协议转换为统一模型；
//! `core` 负责把统一模型映射到 CoreService 所需的请求和文本协议。

mod core;
mod model;
pub(crate) mod qq_official;

pub(crate) use core::{core_scope_key, render_text_for_core, to_core_request};
pub(crate) use model::Platform;
