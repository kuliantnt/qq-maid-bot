//! Agent Loop 尺寸诊断的纯逻辑与 DEBUG 门控日志。
//!
//! Issue #361 只关心“上下文是否台阶式放大”，因此本模块只输出尺寸、计数与
//! 进程内存读数，绝不输出聊天正文、工具输出正文、知识正文或鉴权信息。
//!
//! 两个口径的区别：
//! - [`tool_result_chars`]：本轮工具结果的**独立**体积（不依赖会话状态）。
//! - [`log_input_size_after_append`]：Provider `advance` 里
//!   `append_tool_results` 之后、payload 构造之前的会话真实输入尺寸。
//!
//! 所有序列化估算都放在 DEBUG 门控内：默认 INFO 级别不生成完整 String 副本。

use tracing::debug;

use super::session::AgentInputSizeEstimate;
use super::types::AgentToolResult;

/// 统计一批工具结果的输出字符总数。
///
/// `AgentToolResult.output` 本身就是 `String`，直接数 `char` 即可，不再经过
/// `to_string()` 生成一份完整副本（DEBUG 未开启时也不会做任何序列化）。
pub(crate) fn tool_result_chars(results: &[AgentToolResult]) -> usize {
    results
        .iter()
        .map(|result| result.output.chars().count())
        .sum()
}

/// 在 `append_tool_results` 之后、payload 构造之前记录会话真实输入尺寸。
///
/// 只应在 DEBUG 级别输出；`AgentInputSizeEstimate` 的 `estimated_chars` 字段
/// 自身也只在 DEBUG 开启时才会做序列化估算，避免默认级别为诊断复制上下文。
pub(crate) fn log_input_size_after_append(
    provider: &str,
    model: &str,
    estimate: AgentInputSizeEstimate,
) {
    let mem = qq_maid_common::process_mem::process_memory_sample();
    debug!(
        provider = provider,
        model = %model,
        input_item_count = estimate.item_count,
        input_estimated_chars = estimate.estimated_chars,
        input_tool_result_chars = estimate.tool_result_chars,
        rss_kb = mem.rss_kb,
        vm_size_kb = mem.vm_size_kb,
        pss_kb = mem.pss_kb,
        private_dirty_kb = mem.private_dirty_kb,
        "agent_loop_input_after_append"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_chars_counts_output_without_extra_copy() {
        let results = vec![
            AgentToolResult {
                call_id: "call-1".to_owned(),
                output: "结果正文".repeat(10),
            },
            AgentToolResult {
                call_id: "call-2".to_owned(),
                output: String::new(),
            },
        ];
        assert_eq!(
            tool_result_chars(&results),
            "结果正文".repeat(10).chars().count()
        );
        assert_eq!(tool_result_chars(&[]), 0);
    }
}
