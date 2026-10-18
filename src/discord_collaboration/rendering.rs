use super::*;

/// Bound each part by UTF-16 units, preserving Unicode and preferring line breaks.
pub fn render_delivery(delivery: &Delivery, checkout: &Path) -> Vec<Delivery> {
    let text = crate::conversation::sanitize_provider_reply(&delivery.text, checkout);
    if text.encode_utf16().count() <= 1900 {
        return vec![Delivery {
            text,
            ..delivery.clone()
        }];
    }
    let mut chunks = Vec::new();
    let mut remaining = text.as_str();
    while !remaining.is_empty() {
        let mut units = 0;
        let mut end = 0;
        let mut line_end = None;
        for (offset, character) in remaining.char_indices() {
            if units + character.len_utf16() > 1800 {
                break;
            }
            units += character.len_utf16();
            end = offset + character.len_utf8();
            if character == '\n' {
                line_end = Some(end);
            }
        }
        if end < remaining.len() {
            end = line_end.filter(|n| *n > 0).unwrap_or(end);
        }
        chunks.push(remaining[..end].to_owned());
        remaining = &remaining[end..];
    }
    let count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, text)| Delivery {
            id: format!("{}:part:{index:06}", delivery.id),
            channel: delivery.channel.clone(),
            text: format!("Part {}/{}\n\n{text}", index + 1, count),
            message_id: None,
            approval: if index + 1 == count {
                delivery.approval.clone()
            } else {
                None
            },
        })
        .collect()
}
pub(super) fn prepare_deliveries(thread: &mut Thread, checkout: &Path) {
    thread.deliveries = thread
        .deliveries
        .drain(..)
        .flat_map(|delivery| {
            if delivery.message_id.is_some() {
                vec![delivery]
            } else {
                render_delivery(&delivery, checkout)
            }
        })
        .collect();
}
