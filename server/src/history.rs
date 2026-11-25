//! The clipboard history's endpoints: sealed items in, sealed items out,
//! only under the vault's current header (the key that sealed them).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use clipboardx_sync_proto::history::{
    HistoryPage, HistoryPush, HistoryPushed, HistoryUp, MAX_CIPHERTEXT, MAX_PUSH_ITEMS,
    MIN_CIPHERTEXT, NONCE_LEN, PAGE_BYTES, valid_id,
};
use serde::Deserialize;

use crate::store::Store;
use crate::{ApiResult, AppState, Authed, error};

#[derive(Deserialize)]
pub(crate) struct PageQuery {
    #[serde(default)]
    since: u64,
    header: u64,
}

/// Refuses a client whose key is not the current header's: what it sealed
/// or would open is under another key.
fn same_header(store: &Store, header: u64) -> ApiResult<()> {
    if header == store.header_version()? {
        Ok(())
    } else {
        Err(error(StatusCode::CONFLICT, "header changed"))
    }
}

fn check(record: &HistoryUp) -> ApiResult<()> {
    let bad = |why| Err(error(StatusCode::BAD_REQUEST, why));
    if !valid_id(&record.id) {
        return bad("bad item id");
    }
    if record.nonce.0.len() != NONCE_LEN {
        return bad("bad nonce");
    }
    if record.ciphertext.0.len() < MIN_CIPHERTEXT {
        return bad("item too short");
    }
    if record.ciphertext.0.len() > MAX_CIPHERTEXT {
        return bad("item too large");
    }
    Ok(())
}

pub(crate) async fn page(
    State(state): State<Arc<AppState>>,
    _: Authed,
    Query(query): Query<PageQuery>,
) -> ApiResult<Json<HistoryPage>> {
    let store = state.store();
    same_header(&store, query.header)?;
    Ok(Json(store.history_page(query.since, PAGE_BYTES)?))
}

pub(crate) async fn push(
    State(state): State<Arc<AppState>>,
    _: Authed,
    Json(request): Json<HistoryPush>,
) -> ApiResult<Json<HistoryPushed>> {
    if request.records.len() > MAX_PUSH_ITEMS {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "too many items"));
    }
    request.records.iter().try_for_each(check)?;
    let (seq, vault_seq) = {
        let mut store = state.store();
        same_header(&store, request.header_version)?;
        if store.history_bytes_with(&request.records)? > state.config.history_bytes {
            log::warn!("history storage full: an upload was refused");
            return Err(error(
                StatusCode::INSUFFICIENT_STORAGE,
                "history storage full",
            ));
        }
        (store.push_history(&request.records)?, store.seq()?)
    };
    // Devices sync on any event: the vault's number, unchanged, wakes them.
    if !request.records.is_empty() {
        state.notify(vault_seq);
    }
    Ok(Json(HistoryPushed { seq }))
}
