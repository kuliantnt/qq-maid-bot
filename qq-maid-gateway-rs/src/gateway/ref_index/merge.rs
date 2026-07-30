use qq_maid_common::input_part::{MediaStatus, MessageInputPart};

/// 被动观察只把索引正文视为权威内容；媒体序列与可读性以当前引用事件为先。
///
/// QQ 引用 payload 经过下载后已经清除了临时 URL。这里对索引和事件媒体做稳定身份的
/// 顺序合并：同一媒体选择可读版本，事件独有媒体保留，索引独有媒体仍作为不可读摘要保留。
/// 索引文字替代事件展示文字，避免同一正文重复或重新引入平台污染文本。
pub(super) fn merge_passive_observation_parts(
    indexed_parts: &[MessageInputPart],
    event_parts: Vec<MessageInputPart>,
) -> Vec<MessageInputPart> {
    if event_parts.is_empty() {
        return indexed_parts.to_vec();
    }

    let (indexed_text_gaps, indexed_media) = split_indexed_parts(indexed_parts);
    if indexed_media.is_empty() {
        return merge_text_only_index(&indexed_text_gaps[0], event_parts);
    }
    let (event_text_gaps, event_media) = split_indexed_parts(&event_parts);
    if event_media.is_empty() {
        return indexed_parts.to_vec();
    }

    let matches = media_lcs_table(&indexed_media, &event_media);
    let mut merged = Vec::with_capacity(indexed_parts.len().saturating_add(event_media.len()));
    let mut indexed_position = 0;
    let mut event_position = 0;
    let mut emitted_index_text_gaps = vec![false; indexed_text_gaps.len()];

    while indexed_position < indexed_media.len() && event_position < event_media.len() {
        if same_media_identity(
            &indexed_media[indexed_position],
            &event_media[event_position],
        ) {
            append_index_text_gap(
                &mut merged,
                &indexed_text_gaps,
                &mut emitted_index_text_gaps,
                indexed_position,
            );
            merged.push(prefer_readable_media_part(
                &indexed_media[indexed_position],
                &event_media[event_position],
            ));
            indexed_position += 1;
            event_position += 1;
        } else if matches[indexed_position + 1][event_position]
            >= matches[indexed_position][event_position + 1]
        {
            append_index_text_gap(
                &mut merged,
                &indexed_text_gaps,
                &mut emitted_index_text_gaps,
                indexed_position,
            );
            merged.push(indexed_media[indexed_position].clone());
            indexed_position += 1;
        } else {
            if !event_text_gaps[event_position].is_empty() {
                append_index_text_gap(
                    &mut merged,
                    &indexed_text_gaps,
                    &mut emitted_index_text_gaps,
                    indexed_position,
                );
            }
            merged.push(event_media[event_position].clone());
            event_position += 1;
        }
    }
    while indexed_position < indexed_media.len() {
        append_index_text_gap(
            &mut merged,
            &indexed_text_gaps,
            &mut emitted_index_text_gaps,
            indexed_position,
        );
        merged.push(indexed_media[indexed_position].clone());
        indexed_position += 1;
    }
    while event_position < event_media.len() {
        if !event_text_gaps[event_position].is_empty() {
            append_index_text_gap(
                &mut merged,
                &indexed_text_gaps,
                &mut emitted_index_text_gaps,
                indexed_position,
            );
        }
        merged.push(event_media[event_position].clone());
        event_position += 1;
    }
    if !event_text_gaps[event_media.len()].is_empty() {
        append_index_text_gap(
            &mut merged,
            &indexed_text_gaps,
            &mut emitted_index_text_gaps,
            indexed_media.len(),
        );
    }
    append_index_text_gap(
        &mut merged,
        &indexed_text_gaps,
        &mut emitted_index_text_gaps,
        indexed_media.len(),
    );
    merged
}

fn append_index_text_gap(
    merged: &mut Vec<MessageInputPart>,
    indexed_text_gaps: &[Vec<MessageInputPart>],
    emitted: &mut [bool],
    position: usize,
) {
    if !emitted[position] {
        merged.extend(indexed_text_gaps[position].iter().cloned());
        emitted[position] = true;
    }
}

fn merge_text_only_index(
    indexed_text: &[MessageInputPart],
    event_parts: Vec<MessageInputPart>,
) -> Vec<MessageInputPart> {
    let mut merged = Vec::with_capacity(indexed_text.len().saturating_add(event_parts.len()));
    let mut inserted_text = false;
    for part in event_parts {
        if part.is_non_text() {
            merged.push(part);
        } else if !inserted_text {
            merged.extend(indexed_text.iter().cloned());
            inserted_text = true;
        }
    }
    if !inserted_text {
        merged.splice(0..0, indexed_text.iter().cloned());
    }
    merged
}

fn split_indexed_parts(
    indexed_parts: &[MessageInputPart],
) -> (Vec<Vec<MessageInputPart>>, Vec<MessageInputPart>) {
    let media_count = indexed_parts
        .iter()
        .filter(|part| part.is_non_text())
        .count();
    let mut text_gaps = vec![Vec::new(); media_count + 1];
    let mut media = Vec::with_capacity(media_count);
    for part in indexed_parts {
        if part.is_non_text() {
            media.push(part.clone());
        } else {
            text_gaps[media.len()].push(part.clone());
        }
    }
    (text_gaps, media)
}

fn media_lcs_table(
    indexed_media: &[MessageInputPart],
    event_media: &[MessageInputPart],
) -> Vec<Vec<usize>> {
    let mut table = vec![vec![0; event_media.len() + 1]; indexed_media.len() + 1];
    for indexed_position in (0..indexed_media.len()).rev() {
        for event_position in (0..event_media.len()).rev() {
            table[indexed_position][event_position] = if same_media_identity(
                &indexed_media[indexed_position],
                &event_media[event_position],
            ) {
                table[indexed_position + 1][event_position + 1] + 1
            } else {
                table[indexed_position + 1][event_position]
                    .max(table[indexed_position][event_position + 1])
            };
        }
    }
    table
}

fn same_media_identity(left: &MessageInputPart, right: &MessageInputPart) -> bool {
    if std::mem::discriminant(left) != std::mem::discriminant(right) {
        return false;
    }
    let (Some(left), Some(right)) = (left.media(), right.media()) else {
        return false;
    };
    [
        (
            left.attachment_id.as_deref(),
            right.attachment_id.as_deref(),
        ),
        (left.file_id.as_deref(), right.file_id.as_deref()),
        (left.media_id.as_deref(), right.media_id.as_deref()),
    ]
    .into_iter()
    .any(|(left, right)| non_empty_equal(left, right))
        || match (
            non_empty(left.filename.as_deref()),
            non_empty(right.filename.as_deref()),
        ) {
            (Some(left), Some(right)) => left.eq_ignore_ascii_case(right),
            _ => false,
        }
}

fn non_empty_equal(left: Option<&str>, right: Option<&str>) -> bool {
    match (non_empty(left), non_empty(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn prefer_readable_media_part(
    indexed_part: &MessageInputPart,
    event_part: &MessageInputPart,
) -> MessageInputPart {
    if media_part_is_readable(event_part) || !media_part_is_readable(indexed_part) {
        event_part.clone()
    } else {
        indexed_part.clone()
    }
}

fn media_part_is_readable(part: &MessageInputPart) -> bool {
    part.media().is_some_and(|media| {
        media.status == MediaStatus::Available
            && (non_empty(media.local_path.as_deref()).is_some() || media.remote_url().is_some())
    })
}
