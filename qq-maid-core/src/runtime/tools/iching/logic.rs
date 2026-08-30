//! 起卦的纯函数、卦象映射和变卦计算。

use super::data::{HexagramText, SpecialText, hexagram_by_code};

const ALL_LINES_MASK: u8 = 0b11_1111;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CastLine {
    pub(crate) value: u8,
    pub(crate) yang: bool,
    pub(crate) moving: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidCastValue {
    pub(crate) position: usize,
    pub(crate) value: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CastResult {
    pub(crate) values: [u8; 6],
    pub(crate) lines: [CastLine; 6],
    pub(crate) original: &'static HexagramText,
    pub(crate) changed: Option<&'static HexagramText>,
    pub(crate) moving_mask: u8,
    pub(crate) special: Option<&'static SpecialText>,
}

/// 将六轮结果按“第一次为初爻、最后一次为上爻”计算本卦与之卦。
pub(crate) fn calculate_cast(values: [u8; 6]) -> Result<CastResult, InvalidCastValue> {
    let mut original_code = 0;
    let mut changed_code = 0;
    let mut moving_mask = 0;
    let mut lines = [CastLine {
        value: 0,
        yang: false,
        moving: false,
    }; 6];

    for (position, value) in values.into_iter().enumerate() {
        let (yang, moving, changed_yang) = match value {
            6 => (false, true, true),
            7 => (true, false, true),
            8 => (false, false, false),
            9 => (true, true, false),
            value => return Err(InvalidCastValue { position, value }),
        };
        // 初爻位于本组三位编码的高位；上卦还要整体移到 code 的高三位。
        let trigram_bit = 2 - (position % 3) + (position / 3) * 3;
        original_code |= u8::from(yang) << trigram_bit;
        changed_code |= u8::from(changed_yang) << trigram_bit;
        if moving {
            moving_mask |= 1 << position;
        }
        lines[position] = CastLine {
            value,
            yang,
            moving,
        };
    }

    let original = hexagram_by_code(original_code);
    let changed = (moving_mask != 0).then(|| hexagram_by_code(changed_code));
    let special = (moving_mask == ALL_LINES_MASK)
        .then_some(original.special.as_ref())
        .flatten();

    Ok(CastResult {
        values,
        lines,
        original,
        changed,
        moving_mask,
        special,
    })
}
