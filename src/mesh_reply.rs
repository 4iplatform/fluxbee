//! Mesh replies that fit the frame (FINDINGS A-198).
//!
//! The SDK refuses a frame over `MAX_FRAME_SIZE` (128 KiB) before it sends it, and the node that
//! asked waits to its own timeout with no reason. Two rules keep an answer from going missing:
//!
//! - **A reply that does not fit is answered all the same.** When a node's send of a reply fails
//!   with `NodeError::MessageTooLarge`, the node sends `REPLY_TOO_LARGE` in its place: the reply's
//!   size, the frame's, and the reply's own outcome (`reply_outcome`: its status, its error code
//!   and the ids it can be read back by), so an action that ran is never taken for one that did
//!   not.
//! - **A listing that grows with the installation answers in pages.** Asked with `limit` or
//!   `cursor` (`PageAsk`), a node answers the rows after `cursor`, at most `limit` of them and at
//!   most `LIST_PAGE_MAX_BYTES`, with `next_cursor` while rows follow and `total`, the rows the
//!   listing holds in all (`list_page`). Asked with neither, it answers every row, as before.

use std::io;

use serde_json::{json, Map, Value};

pub use fluxbee_sdk::socket::connection::MAX_FRAME_SIZE;

/// The error code of a reply that did not fit the frame, sent in a small reply's place.
pub const REPLY_TOO_LARGE: &str = "REPLY_TOO_LARGE";

/// The most bytes of rows one page of a listing carries: half a frame, so the rest of the answer
/// and the envelope SY.admin wraps it in fit with room to spare.
pub const LIST_PAGE_MAX_BYTES: usize = 64 * 1024;

/// The ids a reply's outcome keeps, from its top level or its `payload`, when one is a short
/// scalar: what the caller reads the action back by.
const OUTCOME_ID_FIELDS: &[&str] = &[
    "request_id",
    "execution_id",
    "operation_id",
    "ilk_id",
    "tenant_id",
    "node_name",
    "hive_id",
    "target",
    "key",
    "version",
    "runtime",
    "runtime_version",
    "workflow_name",
    "instance_id",
    "publication_id",
    "generation",
    "manifest_version",
    "timer_uuid",
];

/// The longest id text an outcome keeps.
const OUTCOME_ID_MAX_CHARS: usize = 256;

/// The bytes `value` takes serialized, as a frame carries it.
pub fn serialized_len(value: &Value) -> usize {
    struct Count(usize);
    impl io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    match serde_json::to_writer(&mut count, value) {
        Ok(()) => count.0,
        Err(_) => 0,
    }
}

/// The outcome of `reply`, a node's answer or SY.admin's envelope: its `status` and `error_code`,
/// and the ids it names (`OUTCOME_ID_FIELDS`), at its top level or in its `payload`.
pub fn reply_outcome(reply: &Value) -> Value {
    let mut outcome = Map::new();
    for key in ["status", "error_code"] {
        if let Some(value) = reply.get(key).filter(short_scalar) {
            outcome.insert(key.to_string(), value.clone());
        }
    }
    for source in [Some(reply), reply.get("payload")].into_iter().flatten() {
        for key in OUTCOME_ID_FIELDS {
            if outcome.contains_key(*key) {
                continue;
            }
            if let Some(value) = source.get(*key).filter(short_scalar) {
                outcome.insert((*key).to_string(), value.clone());
            }
        }
    }
    Value::Object(outcome)
}

/// Whether `value` is what an outcome keeps: a number, a boolean, or a text of at most 256
/// characters, never an object, a list or a long text.
pub fn is_outcome_value(value: &Value) -> bool {
    short_scalar(&value)
}

fn short_scalar(value: &&Value) -> bool {
    match value {
        Value::String(text) => !text.is_empty() && text.chars().count() <= OUTCOME_ID_MAX_CHARS,
        Value::Number(_) | Value::Bool(_) => true,
        _ => false,
    }
}

/// What a node answers in place of a reply of `size` bytes the frame refused, in its own replies'
/// shape (`status`, `error_code`, `message`): the sizes and the reply's `outcome`
/// (`reply_outcome`, taken before the send took the reply). `request` names what was asked (its
/// message or action); `how` says how to get the answer.
pub fn reply_too_large_payload(
    request: &str,
    size: usize,
    max: usize,
    outcome: Value,
    how: &str,
) -> Value {
    json!({
        "status": "error",
        "error_code": REPLY_TOO_LARGE,
        "message": format!(
            "the answer to {request} is {size} bytes, past the mesh frame's {max}: {how}"
        ),
        "size": size,
        "max": max,
        "outcome": outcome,
    })
}

/// What a caller asks of a listing: the rows after `cursor` (the `next_cursor` of the page
/// before), at most `limit` of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageAsk {
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

impl PageAsk {
    /// `limit` and `cursor` of a request: an absent or null field is none, and so is an empty
    /// cursor. A limit is a whole number from 1 (a query string's text of one too); anything else
    /// is refused, with why.
    pub fn from_payload(payload: &Value) -> Result<Self, String> {
        let limit = match payload.get("limit") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let rows = match value {
                    Value::Number(number) => number.as_u64(),
                    Value::String(text) => text.trim().parse::<u64>().ok(),
                    _ => None,
                };
                match rows.filter(|rows| *rows >= 1) {
                    Some(rows) => Some(usize::try_from(rows).unwrap_or(usize::MAX)),
                    None => {
                        return Err(format!("limit must be a whole number from 1, got {value}"))
                    }
                }
            }
        };
        let cursor = match payload.get("cursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(cursor)) => {
                Some(cursor.trim().to_string()).filter(|cursor| !cursor.is_empty())
            }
            Some(other) => {
                return Err(format!(
                    "cursor must be the next_cursor of the page before, a string, got {other}"
                ))
            }
        };
        Ok(Self { limit, cursor })
    }

    /// Whether the caller asked for a page, not for the whole listing.
    pub fn is_page(&self) -> bool {
        self.limit.is_some() || self.cursor.is_some()
    }

    /// `request` with this ask's `limit` and `cursor`, and without the ones it does not name.
    pub fn apply_to(&self, request: &mut Value) {
        let Some(fields) = request.as_object_mut() else {
            return;
        };
        match self.limit {
            Some(limit) => fields.insert("limit".to_string(), json!(limit)),
            None => fields.remove("limit"),
        };
        match &self.cursor {
            Some(cursor) => fields.insert("cursor".to_string(), json!(cursor)),
            None => fields.remove("cursor"),
        };
    }
}

/// One page of a listing (`list_page`).
#[derive(Debug, Clone, PartialEq)]
pub struct ListPage {
    pub rows: Vec<Value>,
    /// The key of the page's last row, while rows follow it.
    pub next_cursor: Option<String>,
    /// The rows the listing holds in all, every page's.
    pub total: usize,
}

impl ListPage {
    /// The page's `next_cursor` (only while rows follow) and `total` on its listing's `answer`.
    pub fn mark(&self, answer: &mut Value) {
        let Some(fields) = answer.as_object_mut() else {
            return;
        };
        match &self.next_cursor {
            Some(cursor) => fields.insert("next_cursor".to_string(), json!(cursor)),
            None => fields.remove("next_cursor"),
        };
        fields.insert("total".to_string(), json!(self.total));
    }

    /// `answer` with the page's rows under `rows_key`, and the page's `next_cursor` and `total`
    /// (`mark`).
    pub fn into_answer(self, rows_key: &str, mut answer: Value) -> Value {
        self.mark(&mut answer);
        answer[rows_key] = Value::Array(self.rows);
        answer
    }
}

/// One page of a listing. `rows` is every row the listing holds, in its order, each with its key,
/// unique in the listing; `after(key, cursor)` says whether a row comes after the one `cursor`
/// names in that order, which holds even when that row has gone since. Not asked for a page, every
/// row; asked, the rows after the cursor, at most `limit` of them and at most `max_bytes`
/// serialized, though always one when one follows, so every page moves on.
pub fn list_page<F>(
    rows: Vec<(String, Value)>,
    ask: &PageAsk,
    max_bytes: usize,
    after: F,
) -> ListPage
where
    F: Fn(&str, &str) -> bool,
{
    let total = rows.len();
    if !ask.is_page() {
        return ListPage {
            rows: rows.into_iter().map(|(_, row)| row).collect(),
            next_cursor: None,
            total,
        };
    }
    let mut page = Vec::new();
    let mut bytes = 0usize;
    let mut last_key = None;
    let mut more = false;
    for (key, row) in rows {
        if ask
            .cursor
            .as_deref()
            .is_some_and(|cursor| !after(&key, cursor))
        {
            continue;
        }
        let size = serialized_len(&row) + 1;
        if ask.limit.is_some_and(|limit| page.len() >= limit)
            || (!page.is_empty() && bytes + size > max_bytes)
        {
            more = true;
            break;
        }
        bytes += size;
        last_key = Some(key);
        page.push(row);
    }
    ListPage {
        rows: page,
        next_cursor: if more { last_key } else { None },
        total,
    }
}

/// `value` cut to about `max_bytes` serialized, its shape kept: an object keeps every field, the
/// small ones whole and the large ones cut to their share (smallest first, so a status, an id or a
/// short list stays as it is); an array keeps the items that fit and counts the rest
/// (`{"truncated_items": n}`); a string keeps its start and says how many bytes it lost. Numbers,
/// booleans and null stay. A value already within `max_bytes` comes back as it is.
pub fn bounded_value(value: &Value, max_bytes: usize) -> Value {
    let size = serialized_len(value);
    if size <= max_bytes {
        return value.clone();
    }
    // Too little room for any of its shape: what it was, in a few bytes.
    if max_bytes < 48 {
        return json!(format!("[{size} bytes cut]"));
    }
    match value {
        Value::String(text) => {
            // The quotes and the note of what was cut, at its longest.
            let note = format!("…[{} bytes cut]", text.len()).len() + 2;
            let mut keep = max_bytes.saturating_sub(note);
            while keep > 0 && !text.is_char_boundary(keep) {
                keep -= 1;
            }
            let keep = keep.min(text.len());
            json!(format!(
                "{}…[{} bytes cut]",
                &text[..keep],
                text.len() - keep
            ))
        }
        Value::Array(items) => {
            // Room for the count of what was cut.
            let mut left = max_bytes.saturating_sub(2 + 32);
            let mut kept = Vec::new();
            for item in items {
                let item_size = serialized_len(item) + 1;
                if item_size > left {
                    break;
                }
                left -= item_size;
                kept.push(item.clone());
            }
            let cut = items.len() - kept.len();
            kept.push(json!({ "truncated_items": cut }));
            Value::Array(kept)
        }
        Value::Object(fields) => {
            let mut by_size: Vec<(&String, &Value, usize)> = fields
                .iter()
                .map(|(key, field)| (key, field, serialized_len(field)))
                .collect();
            by_size.sort_by_key(|(_, _, size)| *size);
            let count = by_size.len();
            let mut left = max_bytes.saturating_sub(2);
            let mut kept = Map::new();
            for (index, (key, field, field_size)) in by_size.into_iter().enumerate() {
                let overhead = key.len() + 4;
                let share = (left / (count - index)).saturating_sub(overhead);
                let field = if field_size <= share {
                    field.clone()
                } else {
                    bounded_value(field, share)
                };
                left = left.saturating_sub(serialized_len(&field) + overhead);
                kept.insert(key.clone(), field);
            }
            Value::Object(kept)
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(ids: &[&str]) -> Vec<(String, Value)> {
        ids.iter()
            .map(|id| (id.to_string(), json!({ "id": id })))
            .collect()
    }

    fn ids(page: &ListPage) -> Vec<&str> {
        page.rows
            .iter()
            .map(|row| row["id"].as_str().expect("id"))
            .collect()
    }

    #[test]
    fn a_listing_not_asked_for_a_page_answers_every_row() {
        let page = list_page(
            keyed(&["a", "b", "c"]),
            &PageAsk::default(),
            1,
            |key, cursor| key > cursor,
        );
        assert_eq!(ids(&page), vec!["a", "b", "c"]);
        assert_eq!(page.next_cursor, None);
        assert_eq!(page.total, 3);
    }

    #[test]
    fn pages_follow_their_cursor_to_the_end() {
        let rows = || keyed(&["a", "b", "c", "d", "e"]);
        let after = |key: &str, cursor: &str| key > cursor;
        let first = list_page(
            rows(),
            &PageAsk {
                limit: Some(2),
                cursor: None,
            },
            LIST_PAGE_MAX_BYTES,
            after,
        );
        assert_eq!(ids(&first), vec!["a", "b"]);
        assert_eq!(first.next_cursor.as_deref(), Some("b"));
        assert_eq!(first.total, 5);
        let second = list_page(
            rows(),
            &PageAsk {
                limit: Some(2),
                cursor: first.next_cursor.clone(),
            },
            LIST_PAGE_MAX_BYTES,
            after,
        );
        assert_eq!(ids(&second), vec!["c", "d"]);
        // A row gone since the cursor was given does not lose the place.
        let without_d = keyed(&["a", "b", "c", "e"]);
        let last = list_page(
            without_d,
            &PageAsk {
                limit: Some(2),
                cursor: Some("d".to_string()),
            },
            LIST_PAGE_MAX_BYTES,
            after,
        );
        assert_eq!(ids(&last), vec!["e"]);
        assert_eq!(last.next_cursor, None);
    }

    #[test]
    fn a_page_stays_within_its_bytes_and_still_moves_on() {
        let big = |id: &str| (id.to_string(), json!({ "id": id, "blob": "x".repeat(40) }));
        let rows = vec![big("a"), big("b"), big("c")];
        let one_row = serialized_len(&rows[0].1) + 1;
        let page = list_page(
            rows.clone(),
            &PageAsk {
                limit: Some(10),
                cursor: None,
            },
            2 * one_row,
            |key, cursor| key > cursor,
        );
        assert_eq!(ids(&page), vec!["a", "b"]);
        assert_eq!(page.next_cursor.as_deref(), Some("b"));
        // A row larger than the page alone still goes, so a page never comes back empty.
        let tiny = list_page(
            rows,
            &PageAsk {
                limit: Some(10),
                cursor: None,
            },
            1,
            |key, cursor| key > cursor,
        );
        assert_eq!(ids(&tiny), vec!["a"]);
        assert_eq!(tiny.next_cursor.as_deref(), Some("a"));
    }

    #[test]
    fn a_page_ask_reads_limit_and_cursor_and_refuses_what_is_neither() {
        assert_eq!(
            PageAsk::from_payload(&json!({ "limit": 5, "cursor": "ilk:1" })),
            Ok(PageAsk {
                limit: Some(5),
                cursor: Some("ilk:1".to_string())
            })
        );
        assert_eq!(
            PageAsk::from_payload(&json!({ "limit": "20", "cursor": " " })),
            Ok(PageAsk {
                limit: Some(20),
                cursor: None
            })
        );
        assert_eq!(
            PageAsk::from_payload(&json!({ "limit": null, "cursor": null })),
            Ok(PageAsk::default())
        );
        assert!(PageAsk::from_payload(&json!({ "limit": 0 })).is_err());
        assert!(PageAsk::from_payload(&json!({ "limit": "many" })).is_err());
        assert!(PageAsk::from_payload(&json!({ "cursor": 7 })).is_err());
        let mut request = json!({ "include_deleted": true, "limit": 9, "cursor": "old" });
        PageAsk {
            limit: Some(3),
            cursor: None,
        }
        .apply_to(&mut request);
        assert_eq!(request, json!({ "include_deleted": true, "limit": 3 }));
    }

    #[test]
    fn an_outcome_keeps_the_status_the_error_and_the_ids() {
        let envelope = json!({
            "status": "ok",
            "error_code": null,
            "payload": {
                "execution_id": "exec-1",
                "events": ["x"],
                "tenant_id": "t".repeat(OUTCOME_ID_MAX_CHARS + 1),
            },
        });
        assert_eq!(
            reply_outcome(&envelope),
            json!({ "status": "ok", "execution_id": "exec-1" })
        );
        let answer = json!({ "status": "error", "error_code": "BUSY", "version": 7 });
        let small = reply_too_large_payload(
            "VAULT_LIST",
            200_000,
            MAX_FRAME_SIZE,
            reply_outcome(&answer),
            "page it",
        );
        assert_eq!(small["error_code"], json!(REPLY_TOO_LARGE));
        assert_eq!(
            small["outcome"],
            json!({ "status": "error", "error_code": "BUSY", "version": 7 })
        );
    }

    #[test]
    fn a_bounded_value_keeps_its_small_fields_whole() {
        let value = json!({
            "status": "ok",
            "payload": {
                "wf_node": { "status": "ok", "node_name": "WF.a@motherbee" },
                "pending": ["worker1"],
                "definition": "d".repeat(20_000),
                "rows": (0..500).map(|n| json!({ "n": n })).collect::<Vec<_>>(),
            },
        });
        let bounded = bounded_value(&value, 2_048);
        assert!(serialized_len(&bounded) <= 2_048 + 64, "{bounded}");
        assert_eq!(bounded["status"], json!("ok"));
        assert_eq!(bounded["payload"]["wf_node"], value["payload"]["wf_node"]);
        assert_eq!(bounded["payload"]["pending"], json!(["worker1"]));
        let definition = bounded["payload"]["definition"].as_str().expect("text");
        assert!(definition.ends_with("bytes cut]"), "{definition}");
        let rows = bounded["payload"]["rows"].as_array().expect("rows");
        assert!(rows.last().expect("count")["truncated_items"].as_u64() > Some(0));
        // A value within its bytes is not touched.
        assert_eq!(
            bounded_value(&value["payload"]["wf_node"], 2_048),
            value["payload"]["wf_node"]
        );
    }
}
