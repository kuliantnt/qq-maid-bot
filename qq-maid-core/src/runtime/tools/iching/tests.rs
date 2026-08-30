//! 《周易》起卦纯函数与命令接入测试。

use super::*;

#[test]
fn contains_all_king_wen_hexagrams_with_six_lines() {
    assert_eq!(data::HEXAGRAMS.len(), 64);
    let mut codes = data::HEXAGRAMS
        .iter()
        .map(|hexagram| hexagram.code)
        .collect::<Vec<_>>();
    codes.sort_unstable();
    assert_eq!(codes, (0..64).collect::<Vec<_>>());
    for (index, hexagram) in data::HEXAGRAMS.iter().enumerate() {
        assert_eq!(hexagram.number, u8::try_from(index + 1).unwrap());
        assert_eq!(hexagram.lines.len(), 6);
    }
}

#[test]
fn identifies_hexagram_from_bottom_to_top_values() {
    let result = calculate_cast([7, 7, 7, 8, 8, 8]).unwrap();
    let rendered = render_cast(&result);

    assert_eq!(result.original.short_name, "泰");
    assert_eq!(result.original.number, 11);
    assert_eq!(result.changed, None);
    assert_eq!(result.moving_mask, 0);
    assert!(result.lines[0].yang);
    assert!(!result.lines[5].yang);
    assert!(rendered.find("上六  ⚋").unwrap() < rendered.find("初九  ⚊").unwrap());
}

#[test]
fn encodes_trigrams_from_bottom_to_top_as_high_to_low_bits() {
    let zhen = calculate_cast([7, 8, 8, 8, 8, 8]).unwrap();
    assert_eq!(zhen.original.code, 4);
    assert_eq!(zhen.original.short_name, "复");

    let xun = calculate_cast([8, 7, 7, 8, 8, 8]).unwrap();
    assert_eq!(xun.original.code, 3);
    assert_eq!(xun.original.short_name, "升");
}

#[test]
fn one_old_yang_changes_the_first_line_into_sheng() {
    let result = calculate_cast([9, 7, 7, 8, 8, 8]).unwrap();
    let rendered = render_cast(&result);

    assert_eq!(result.original.short_name, "泰");
    assert_eq!(result.changed.unwrap().short_name, "升");
    assert_eq!(result.changed.unwrap().number, 46);
    assert_eq!(result.moving_mask, 0b000001);
    assert!(rendered.contains("初九  ⚊ ○"));
    assert!(rendered.contains("动爻：初九"));
    assert!(rendered.contains("之卦：地风升（第46卦）"));
}

#[test]
fn one_old_yin_changes_the_first_line_into_tai() {
    let result = calculate_cast([6, 7, 7, 8, 8, 8]).unwrap();
    let rendered = render_cast(&result);

    assert_eq!(result.original.short_name, "升");
    assert_eq!(result.original.number, 46);
    assert_eq!(result.changed.unwrap().short_name, "泰");
    assert_eq!(result.changed.unwrap().number, 11);
    assert!(rendered.contains("初六  ⚋ ×"));
}

#[test]
fn multiple_moving_lines_are_rendered_in_ascending_position_order() {
    let result = calculate_cast([9, 7, 9, 8, 8, 8]).unwrap();
    let rendered = render_cast(&result);

    assert_eq!(result.original.short_name, "泰");
    assert_eq!(result.changed.unwrap().short_name, "师");
    assert_eq!(result.moving_mask, 0b000101);
    assert!(rendered.contains("动爻：初九、九三"));
    assert!(rendered.find("【初九】").unwrap() < rendered.find("【九三】").unwrap());
}

#[test]
fn no_moving_line_only_renders_the_original_hexagram_text() {
    let result = calculate_cast([7, 7, 7, 8, 8, 8]).unwrap();
    let rendered = render_cast(&result);

    assert!(rendered.contains("【卦辞】"));
    assert!(rendered.contains("起卦方法：三钱法（3d2+3）"));
    assert!(rendered.ends_with("原始数值：7, 7, 7, 8, 8, 8"));
    assert!(!rendered.contains("动爻："));
    assert!(!rendered.contains("【之卦】"));
    assert!(!rendered.contains("【初九】"));
}

#[test]
fn all_changing_qian_and_kun_use_their_special_lines() {
    let qian = calculate_cast([9; 6]).unwrap();
    let qian_text = render_cast(&qian);
    assert_eq!(qian.original.short_name, "乾");
    assert_eq!(qian.changed.unwrap().short_name, "坤");
    assert_eq!(qian.special.unwrap().label, "用九");
    assert!(qian_text.contains("动爻：六爻皆变（用九）"));
    assert!(qian_text.contains("【用九】"));

    let kun = calculate_cast([6; 6]).unwrap();
    let kun_text = render_cast(&kun);
    assert_eq!(kun.original.short_name, "坤");
    assert_eq!(kun.changed.unwrap().short_name, "乾");
    assert_eq!(kun.special.unwrap().label, "用六");
    assert!(kun_text.contains("动爻：六爻皆变（用六）"));
    assert!(kun_text.contains("【用六】"));
}

#[test]
fn invalid_line_value_is_rejected() {
    let error = calculate_cast([7, 7, 5, 8, 8, 8]).unwrap_err();
    assert_eq!(error.position, 2);
    assert_eq!(error.value, 5);
}

#[test]
fn iching_uses_the_existing_roll_parser_and_multi_round_roller() {
    let mut die_values = [
        1, 1, 1, // 6
        1, 1, 2, // 7
        1, 2, 1, // 7
        1, 2, 2, // 8
        2, 1, 2, // 8
        2, 2, 2, // 9
    ]
    .into_iter();
    let mut calls = Vec::new();
    let totals = crate::runtime::tools::roll::roll_local_command_totals_with_roller(
        "/r6#(3d2+3)",
        &mut |sides| {
            calls.push(sides);
            die_values.next().unwrap()
        },
    )
    .unwrap();

    assert_eq!(totals, [6, 7, 7, 8, 8, 9]);
    assert_eq!(calls, vec![2; 18]);
    assert_eq!(parse_iching_command("/算卦"), Some(IChingCommand::Cast));
    assert_eq!(parse_iching_command("/算卦 额外参数"), None);
    assert!(is_iching_command("/算卦 额外参数"));
    assert!(is_iching_command("/iching 额外参数"));
    assert!(!is_iching_command("/天气 杭州"));
}
