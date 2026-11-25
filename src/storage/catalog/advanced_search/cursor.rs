//! Bind advanced search continuation tokens to the authorized query and result snapshot.

use super::{
    super::{MAX_SEARCH_PAGE_TOKEN_BYTES, encode_lower_hex, update_search_fingerprint},
    MatchedEvent, Scope,
};
use crate::storage::search::EventAdvancedSearchQuery;
use anyhow::ensure;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
pub(super) struct Cursor {
    snapshot: String,
    offset: u32,
}

/// Decodes and validates an advanced search continuation token.
pub(super) fn decode(token: &str) -> anyhow::Result<Cursor> {
    ensure!(
        (1..=MAX_SEARCH_PAGE_TOKEN_BYTES).contains(&token.len()),
        "invalid advanced search page token length"
    );
    let raw = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|_| anyhow::anyhow!("invalid advanced search page token"))?;
    let cursor: Cursor = serde_json::from_slice(&raw)
        .map_err(|_| anyhow::anyhow!("invalid advanced search page token"))?;
    ensure!(
        cursor.snapshot.len() == 64 && cursor.offset > 0,
        "invalid advanced search page token"
    );
    Ok(cursor)
}

/// Slices one deterministic page and mints a snapshot-bound token when more hits follow.
pub(super) fn slice_page(
    scope: &Scope<'_>,
    query: &EventAdvancedSearchQuery,
    matches: Vec<MatchedEvent>,
    previous: Option<Cursor>,
) -> anyhow::Result<(Vec<MatchedEvent>, Option<String>)> {
    let snapshot = snapshot_digest(scope, query, &matches);
    let limit = query.page_size;
    let offset = match previous {
        Some(cursor) => {
            ensure!(
                snapshot == cursor.snapshot
                    && cursor.offset.is_multiple_of(limit)
                    && (cursor.offset as usize) < matches.len(),
                "advanced search snapshot changed; restart the query"
            );
            cursor.offset
        }
        None => 0,
    };
    let end = offset.saturating_add(limit);
    let next = ((end as usize) < matches.len())
        .then(|| {
            serde_json::to_vec(&Cursor {
                snapshot,
                offset: end,
            })
            .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        })
        .transpose()?;
    let page_hits = matches
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Ok((page_hits, next))
}

fn snapshot_digest(
    scope: &Scope<'_>,
    query: &EventAdvancedSearchQuery,
    matches: &[MatchedEvent],
) -> String {
    let mut hash = Sha256::new();
    let mut put = |bytes: &[u8]| update_search_fingerprint(&mut hash, bytes);
    put(b"keeppeek-advanced-search-cursor-v1");
    put(&query.expression_version.to_le_bytes());
    put(query.canonical_hash.as_bytes());
    put(&query.policy_revision.to_le_bytes());
    put(query.stream_id.as_bytes());
    put(&scope.start_ms.to_le_bytes());
    put(&scope.end_ms.to_le_bytes());
    put(&query.page_size.to_le_bytes());
    put(&(scope.cameras.len() as u32).to_le_bytes());
    for camera in &scope.cameras {
        put(camera.as_bytes());
    }
    match &query.workflow {
        Some(workflow) => {
            put(b"workflow");
            put(workflow.fingerprint().as_bytes());
        }
        None => put(b"no-workflow"),
    }
    match &query.semantic {
        Some(semantic) => {
            put(b"semantic");
            put(semantic.embedding.model_id.as_bytes());
            put(semantic.model_version.as_bytes());
            put(&(semantic.embedding.values.len() as u32).to_le_bytes());
            for value in &semantic.embedding.values {
                put(&value.to_bits().to_le_bytes());
            }
            if let Some(min_score) = semantic.minimum_score {
                put(&min_score.to_bits().to_le_bytes());
            }
        }
        None => put(b"boolean-only"),
    }
    put(&(matches.len() as u32).to_le_bytes());
    for item in matches {
        put(item.id.as_bytes());
        put(&item.start_ms.to_le_bytes());
        put(&item.revision.to_le_bytes());
        if let Some(distance) = item.distance {
            put(&distance.to_bits().to_le_bytes());
        }
    }
    encode_lower_hex(hash.finalize())
}
