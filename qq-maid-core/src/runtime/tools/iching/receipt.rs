//! 《周易》起卦的确定性用户回执渲染。

use super::{
    data::{HexagramText, TextBlock},
    logic::{CastLine, CastResult},
};

/// 将纯计算结果投影为只包含卦爻原文、简明译文和词义注释的回执。
pub(crate) fn render_cast(result: &CastResult) -> String {
    let yao_lines = (0..6)
        .rev()
        .map(|position| format_line(result.lines[position], position))
        .collect::<Vec<_>>()
        .join("\n");
    let mut sections = vec![format!(
        "🎴 周易起卦\n\n起卦方法：三钱法（3d2+3）\n\n{yao_lines}"
    )];
    sections.push(format!(
        "本卦：{}（第{}卦）",
        result.original.full_name, result.original.number
    ));

    if result.moving_mask != 0 {
        let movement = result
            .special
            .map(|special| format!("六爻皆变（{}）", special.label))
            .unwrap_or_else(|| {
                (0..6)
                    .filter(|position| result.moving_mask & (1 << position) != 0)
                    .map(|position| line_title(result.lines[position].yang, position))
                    .collect::<Vec<_>>()
                    .join("、")
            });
        sections.push(format!("动爻：{movement}"));
        if let Some(changed) = result.changed {
            sections.push(format!(
                "之卦：{}（第{}卦）",
                changed.full_name, changed.number
            ));
        }
    }

    sections.push(render_gua_block(result.original));
    if result.moving_mask != 0 {
        if let Some(special) = result.special {
            sections.push(render_text_block(special.label, &special.text));
        } else {
            for position in 0..6 {
                if result.moving_mask & (1 << position) != 0 {
                    let title = line_title(result.lines[position].yang, position);
                    sections.push(render_text_block(&title, &result.original.lines[position]));
                }
            }
        }
        if let Some(changed) = result.changed {
            sections.push(format!(
                "【之卦】\n{}（第{}卦）\n\n{}",
                changed.full_name,
                changed.number,
                render_gua_block(changed)
            ));
        }
    }
    // 保留固定顺序和原始六轮结果，便于排查随机源、爻位方向与变卦计算问题。
    sections.push(format!(
        "原始数值：{}",
        result
            .values
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ));

    sections.join("\n\n")
}

fn format_line(line: CastLine, position: usize) -> String {
    let title = line_title(line.yang, position);
    let glyph = if line.yang { "⚊" } else { "⚋" };
    let moving = if line.moving { " ○" } else { "" };
    format!("{title}  {glyph} {}{moving}", line.value)
}

fn line_title(yang: bool, position: usize) -> String {
    let ordinal = match position {
        0 => "初",
        1 => "二",
        2 => "三",
        3 => "四",
        4 => "五",
        5 => "上",
        _ => unreachable!("爻位必须在初爻到上爻之间"),
    };
    let kind = if yang { "九" } else { "六" };
    if matches!(position, 0 | 5) {
        format!("{ordinal}{kind}")
    } else {
        format!("{kind}{ordinal}")
    }
}

fn render_gua_block(hexagram: &HexagramText) -> String {
    format!(
        "【卦辞】\n{}：{}\n\n【译文】\n{}\n\n【注释】\n{}",
        hexagram.short_name, hexagram.gua.original, hexagram.gua.translation, hexagram.gua.note
    )
}

fn render_text_block(label: &str, text: &TextBlock) -> String {
    format!(
        "【{label}】\n{}\n\n【译文】\n{}\n\n【注释】\n{}",
        text.original, text.translation, text.note
    )
}
