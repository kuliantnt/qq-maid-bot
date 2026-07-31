//! 进程内存采样工具。
//!
//! 只做诊断用途的进程级内存读数，不参与任何业务判断：
//! - `/proc/self/status`：VmSize / VmRSS（单位本身就是 kB），读取成本极低，
//!   任何阶段都可采样。
//! - `/proc/self/smaps_rollup`：PSS / PrivateDirty，读取成本较高，仅在
//!   `QQ_MAID_MEMORY_DIAGNOSTICS` 显式开启时采样。
//!
//! 不解析 statm 的页数再换算：页大小因架构而异（不能硬编码 4KB），而
//! `/proc/self/status` 的 VmSize / VmRSS 直接以 kB 给出，无需任何换算。
//! 非 Linux 或无法读取时返回 `None` 对应字段，调用方按缺失处理，不阻断主流程。
//! 本模块不输出任何聊天正文、知识正文、搜索正文或鉴权信息。

use std::fs;

/// 一次进程内存采样结果；单位均为 KB。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessMemorySample {
    /// Resident Set Size（KB）；Linux `/proc/self/status` 的 VmRSS。
    pub rss_kb: Option<u64>,
    /// 虚拟地址空间大小（KB）；Linux `/proc/self/status` 的 VmSize。
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
    if let Some((rss, vsize)) = read_status_kb() {
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

/// 从 `/proc/self/status` 读取 VmSize / VmRSS（单位即 kB，无需换算页大小）。
///
/// 任意一行缺失（非 Linux、容器限制或内核裁剪）时整体返回 `None`，不阻断业务。
fn read_status_kb() -> Option<(u64, u64)> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status_to_kb(&status)
}

fn status_to_kb(content: &str) -> Option<(u64, u64)> {
    let mut vm_size = None;
    let mut vm_rss = None;
    for line in content.lines() {
        if let Some(value) = parse_kb_line(line, "VmSize:") {
            vm_size = Some(value);
        } else if let Some(value) = parse_kb_line(line, "VmRSS:") {
            vm_rss = Some(value);
        }
    }
    Some((vm_rss?, vm_size?))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing_reads_vmsize_and_vmrss_in_kb() {
        // /proc/self/status 的 VmSize / VmRSS 本身就是 kB，不涉及页大小换算。
        let content = "\
Name:   test
VmPeak:	  524288 kB
VmSize:	  262144 kB
VmLck:	       0 kB
VmHWM:	   65536 kB
VmRSS:	   32768 kB
RssAnon:   28672 kB
";
        assert_eq!(status_to_kb(content), Some((32768, 262144)));
    }

    #[test]
    fn status_parsing_missing_lines_returns_none() {
        // 非 Linux / 容器裁剪导致关键字段缺失时整体返回 None，不阻塞业务。
        assert_eq!(status_to_kb("Name:   test\nVmRSS:   100 kB\n"), None);
        assert_eq!(status_to_kb("Name:   test\n"), None);
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
