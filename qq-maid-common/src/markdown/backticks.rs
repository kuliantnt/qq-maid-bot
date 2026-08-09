//! Markdown 反引号分隔符的字面量兼容处理。

#[derive(Debug, Clone, Copy)]
struct BacktickRun {
    start: usize,
    end: usize,
    len: usize,
    escaped: bool,
    fence_indented: bool,
    line_suffix_blank: bool,
}

/// 把没有同长度闭合分隔符的反引号 run 转成 Markdown 字面量转义。
///
/// 已闭合的行内代码、围栏代码块与其中使用不同长度分隔符的反引号保持原样。
pub fn escape_unclosed_backticks(text: &str) -> String {
    let runs = collect_backtick_runs(text);
    let mut unmatched = Vec::new();
    let mut index = 0;
    while index < runs.len() {
        if runs[index].escaped {
            index += 1;
            continue;
        }
        let opening = runs[index];
        let fenced_closing = (opening.len >= 3 && opening.fence_indented)
            .then(|| {
                runs[index + 1..].iter().position(|run| {
                    !run.escaped
                        && run.len >= opening.len
                        && run.fence_indented
                        && run.line_suffix_blank
                })
            })
            .flatten();
        let inline_closing = runs[index + 1..]
            .iter()
            .position(|run| !run.escaped && run.len == opening.len);
        let closing = fenced_closing
            .or(inline_closing)
            .map(|offset| index + 1 + offset);
        if let Some(closing) = closing {
            // 不同长度的 run 可以作为已闭合 code span 的正文，不能单独转义。
            index = closing + 1;
        } else {
            unmatched.push(runs[index]);
            index += 1;
        }
    }

    if unmatched.is_empty() {
        return text.to_owned();
    }

    let added_chars = unmatched.iter().map(|run| run.len).sum::<usize>();
    let mut escaped = String::with_capacity(text.len() + added_chars);
    let mut cursor = 0;
    for run in unmatched {
        escaped.push_str(&text[cursor..run.start]);
        for _ in 0..run.len {
            escaped.push_str("\\`");
        }
        cursor = run.end;
    }
    escaped.push_str(&text[cursor..]);
    escaped
}

fn collect_backtick_runs(text: &str) -> Vec<BacktickRun> {
    let bytes = text.as_bytes();
    let mut runs = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }

        let start = index;
        while index < bytes.len() && bytes[index] == b'`' {
            index += 1;
        }
        let preceding_backslashes = bytes[..start]
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count();
        let line_start = text[..start].rfind('\n').map_or(0, |position| position + 1);
        let line_prefix = &text[line_start..start];
        let line_end = text[index..]
            .find('\n')
            .map_or(text.len(), |offset| index + offset);
        runs.push(BacktickRun {
            start,
            end: index,
            len: index - start,
            escaped: preceding_backslashes % 2 == 1,
            fence_indented: line_prefix.len() <= 3 && line_prefix.bytes().all(|byte| byte == b' '),
            line_suffix_blank: text[index..line_end]
                .bytes()
                .all(|byte| matches!(byte, b' ' | b'\t')),
        });
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_only_unclosed_backtick_runs() {
        let cases = [
            ("测试`", "测试\\`"),
            ("`测试", "\\`测试"),
            ("测试`内容", "测试\\`内容"),
            (
                "BAAI/bge-small-zh-v1.5` 这模型配置要求多少",
                "BAAI/bge-small-zh-v1.5\\` 这模型配置要求多少",
            ),
            (
                "`BAAI/bge-small-zh-v1.5` 这模型配置要求多少",
                "`BAAI/bge-small-zh-v1.5` 这模型配置要求多少",
            ),
            ("```rust\nfn main() {}\n```", "```rust\nfn main() {}\n```"),
            ("```rust\nfn main() {}\n````", "```rust\nfn main() {}\n````"),
            ("``code ` tick``", "``code ` tick``"),
            (r"已经转义 \`", r"已经转义 \`"),
        ];

        for (input, expected) in cases {
            assert_eq!(escape_unclosed_backticks(input), expected, "input={input}");
        }
    }
}
