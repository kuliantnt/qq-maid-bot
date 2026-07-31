//! 进程内存采样工具。
//!
//! 只做诊断用途的进程级内存读数，不参与任何业务判断：
//! - `/proc/self/statm`：RSS / VmSize，读取成本极低，任何阶段都可采样。
//! - `/proc/self/smaps_rollup`：PSS / PrivateDirty，读取成本较高，仅在
//!   `QQ_MAID_MEMORY_DIAGNOSTICS` 显式开启时采样。
//!
//! 非 Linux 或无法读取时返回 `None` 对应字段，调用方按缺失处理，不阻断主流程。
//! 本模块不输出任何聊天正文、知识正文、搜索正文或鉴权信息。

use std::fs;

/// 一次进程内存采样结果；单位均为 KB。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessMemorySample {
    /// Resident Set Size（KB）；Linux `/proc/self/statm` 第二列。
    pub rss_kb: Option<u64>,
    /// 虚拟地址空间大小（KB）。
    pub vm_size_kb: Option<u64>,
    /// Proportional Set Size（KB）；仅在显式开启内存诊断时采样。
    pub pss_kb: Option<u64>,
    /// 匿名私有脏页（KB）；仅在显式开启内存诊断时采样。
    pub private_dirty_kb: Option<u64>,
}

impl ProcessMemorySample {
    /// 采样是否完全失败（没有任何字段可读）。
    pub fn is_empty(&self) -> bool {
        self.rss_kb.is_none() && self.vm_size_kb.is_none()
    }
}

/// 内存诊断是否显式开启（`QQ_MAID_MEMORY_DIAGNOSTICS=1`）。
///
/// 该开关只控制 `smaps_rollup` 这类较高成本采样；RSS / VmSize 始终廉价可读。
pub fn memory_diagnostics_enabled() -> bool {
    std::env::var("QQ_MAID_MEMORY_DIAGNOSTICS")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes" | "enabled"
            )
        })
        .unwrap_or(false)
}

/// 读取一次进程内存采样。
pub fn process_memory_sample() -> ProcessMemorySample {
    let mut sample = ProcessMemorySample::default();
    if let Some((rss, vsize)) = read_statm_kb() {
        sample.rss_kb = Some(rss);
        sample.vm_size_kb = Some(vsize);
    }
    if memory_diagnostics_enabled() {
        let (pss, private_dirty) = read_smaps_rollup_kb();
        sample.pss_kb = pss;
        sample.private_dirty_kb = private_dirty;
    }
    sample
}

/// 从 `/proc/self/statm` 读取 RSS / VmSize（单位 KB）。
///
/// statm 各列均以内存页为单位：size resident shared text lib data dt。
fn read_statm_kb() -> Option<(u64, u64)> {
    let statm = fs::read_to_string("/proc/self/statm").ok()?;
    statm_to_kb(&statm, page_size_kb()?)
}

fn statm_to_kb(content: &str, page_size_kb: u64) -> Option<(u64, u64)> {
    let mut fields = content.split_whitespace();
    let page_count = fields.next()?.parse::<u64>().ok()?;
    let resident = fields.next()?.parse::<u64>().ok()?;
    Some((
        resident.saturating_mul(page_size_kb),
        page_count.saturating_mul(page_size_kb),
    ))
}

/// 从 `/proc/self/smaps_rollup` 读取 PSS / PrivateDirty（单位 KB）。
fn read_smaps_rollup_kb() -> (Option<u64>, Option<u64>) {
    let Ok(content) = fs::read_to_string("/proc/self/smaps_rollup") else {
        return (None, None);
    };
    let mut pss = None;
    let mut private_dirty = None;
    for line in content.lines() {
        if let Some(value) = parse_kb_line(line, "Pss:") {
            pss = Some(value);
        } else if let Some(value) = parse_kb_line(line, "Private_Dirty:") {
            private_dirty = Some(value);
        }
    }
    (pss, private_dirty)
}

fn parse_kb_line(line: &str, prefix: &str) -> Option<u64> {
    let value = line.trim_start().strip_prefix(prefix)?.trim();
    value.strip_suffix("kB")?.trim().parse::<u64>().ok()
}

/// 系统内存页大小（KB）。
///
/// 主流 Linux 架构均为 4KB；这里不解析 auxv，直接返回常量，避免诊断路径
/// 引入额外系统调用成本。若将来需要支持非常见页面大小，再改为读取 sysconf。
fn page_size_kb() -> Option<u64> {
    Some(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statm_parsing_uses_page_size() {
        // 1 页 size + 1 页 resident -> 4KB
        assert_eq!(statm_to_kb("1 1 1 1 0 0 0", 4), Some((4, 4)));
    }

    #[test]
    fn smaps_rollup_line_parsing() {
        assert_eq!(
            parse_kb_line("Pss:               1234 kB", "Pss:"),
            Some(1234)
        );
        assert_eq!(
            parse_kb_line("Private_Dirty:       4321 kB", "Private_Dirty:"),
            Some(4321)
        );
        assert_eq!(parse_kb_line("Private_Clean:       99 kB", "Pss:"), None);
    }
}
