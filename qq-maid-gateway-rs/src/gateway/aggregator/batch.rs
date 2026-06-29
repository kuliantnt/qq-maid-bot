use tokio::time::Instant;
use tracing::info;

use super::types::{FlushReason, PendingAggregation};
use crate::{
    config::MessageAggregationConfig,
    gateway::{
        dedupe::{MessageReservation, dedupe_event_key, dedupe_message_key},
        event::C2cMessage,
        logging::mask_scope_key,
    },
    respond::scope_key_from_c2c_message,
};

pub(super) fn append_to_batch(
    batch: &mut PendingAggregation,
    message: C2cMessage,
    reservation: MessageReservation,
    message_chars: usize,
    now: Instant,
    config: &MessageAggregationConfig,
) {
    for id in message_id_values(&message) {
        batch.message_ids.insert(id);
    }
    for id in event_id_values(&message) {
        batch.event_ids.insert(id);
    }
    batch.messages.push(message);
    batch.reservations.push(reservation);
    batch.total_chars += message_chars;
    batch.last_received_at = now;
    batch.quiet_deadline = (now + config.quiet).min(batch.hard_deadline);
    batch.generation = batch.generation.saturating_add(1);
}

pub(super) fn message_id_values(message: &C2cMessage) -> Vec<String> {
    let mut ids = message
        .source_message_ids
        .iter()
        .filter(|id| !id.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if !message.message_id.trim().is_empty() && !ids.iter().any(|id| id == &message.message_id) {
        ids.push(message.message_id.clone());
    }
    ids
}

pub(super) fn event_id_values(message: &C2cMessage) -> Vec<String> {
    let mut ids = message
        .source_event_ids
        .iter()
        .filter(|id| !id.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if let Some(event_id) = message.event_id.as_ref().filter(|id| !id.trim().is_empty())
        && !ids.iter().any(|id| id == event_id)
    {
        ids.push(event_id.clone());
    }
    ids
}

pub(super) fn dedupe_keys(message: &C2cMessage) -> Vec<String> {
    message_id_values(message)
        .into_iter()
        .map(|id| dedupe_message_key(&id))
        .chain(
            event_id_values(message)
                .into_iter()
                .map(|id| dedupe_event_key(&id)),
        )
        .collect()
}

pub(super) fn commit_reservations(reservations: Vec<MessageReservation>) {
    for reservation in reservations {
        reservation.commit();
    }
}

pub(super) fn rollback_reservations(reservations: Vec<MessageReservation>) {
    for reservation in reservations {
        reservation.rollback();
    }
}

pub(super) fn merge_batch(batch: &PendingAggregation, reason: FlushReason) -> C2cMessage {
    let all_messages = batch.messages.clone();
    let mut merged = all_messages
        .last()
        .cloned()
        .expect("aggregation batch should never be empty when flushed");
    merged.content = all_messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    merged.source_message_ids = all_messages.iter().flat_map(message_id_values).collect();
    merged.source_event_ids = all_messages.iter().flat_map(event_id_values).collect();
    merged.first_message_timestamp = all_messages.first().and_then(|message| {
        message
            .first_message_timestamp
            .clone()
            .or_else(|| message.timestamp.clone())
    });
    merged.last_message_timestamp = all_messages.last().and_then(|message| {
        message
            .last_message_timestamp
            .clone()
            .or_else(|| message.timestamp.clone())
    });
    merged.timestamp = merged.last_message_timestamp.clone();
    info!(
        scope_key = %mask_scope_key(&scope_key_from_c2c_message(&merged)),
        aggregation_batch_size = all_messages.len(),
        aggregation_total_chars = batch.total_chars,
        aggregation_wait_ms = batch.first_received_at.elapsed().as_millis() as u64,
        aggregation_flush_reason = reason.as_str(),
        "message aggregation flushed batch"
    );
    merged
}
