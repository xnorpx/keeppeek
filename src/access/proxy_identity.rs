//! Checks identity assertions against the immediate transport peer.

use super::external::{Config, Grant, Method, Provider, Proxy, map_claims};
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use std::{fmt, net::IpAddr};
use subtle::ConstantTimeEq;

// Bound even irrelevant headers before scanning configured assertion names.
const HEADER_LIMIT: usize = 128;
const SUBJECT_BYTES_LIMIT: usize = 256;
const NAME_BYTES_LIMIT: usize = 64;
const SECRET_BYTES_LIMIT: usize = 4_096;

pub struct Assertion {
    pub provider_id: String,
    pub namespace: String,
    pub subject: String,
    pub display_name: String,
    pub grant: Grant,
}

impl fmt::Debug for Assertion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyAssertion").finish_non_exhaustive()
    }
}

pub fn authenticate(
    config: &Config,
    peer: IpAddr,
    headers: &[(&str, &str)],
) -> anyhow::Result<Option<Assertion>> {
    let peer = super::normalize_address(peer);
    let mut selected = None;
    for provider in &config.providers {
        let Method::Proxy(proxy) = &provider.method else {
            continue;
        };
        if proxy
            .trusted_peers
            .iter()
            .any(|network| network.contains(&peer))
        {
            ensure!(
                selected.is_none(),
                "multiple identity providers trust this peer"
            );
            selected = Some((provider, proxy));
        }
    }
    let Some((provider, proxy)) = selected else {
        return Ok(None);
    };
    ensure!(
        headers.len() <= HEADER_LIMIT,
        "too many identity request headers"
    );
    assertion(provider, proxy, headers).map(Some)
}

fn assertion(
    provider: &Provider,
    proxy: &Proxy,
    headers: &[(&str, &str)],
) -> anyhow::Result<Assertion> {
    if let (Some(name), Some(expected)) = (&proxy.secret_header, &proxy.shared_secret) {
        let provided = single_header(headers, name, SECRET_BYTES_LIMIT)?;
        let provided_hash: [u8; 32] = Sha256::digest(provided.as_bytes()).into();
        let expected_hash: [u8; 32] = Sha256::digest(expected.as_bytes()).into();
        ensure!(
            bool::from(provided_hash.ct_eq(&expected_hash)),
            "invalid identity proxy evidence"
        );
    }
    let subject = single_header(headers, &proxy.subject_header, SUBJECT_BYTES_LIMIT)?;
    let role = single_header(headers, &proxy.role_header, SUBJECT_BYTES_LIMIT)?;
    let display_name = match &proxy.name_header {
        Some(name) => single_header(headers, name, NAME_BYTES_LIMIT)?,
        None => &provider.name,
    };
    let grant = map_claims(
        &provider.mappings,
        &serde_json::json!({"sub": subject, "role": role}),
    )?;
    Ok(Assertion {
        provider_id: provider.id.clone(),
        namespace: format!("proxy:{}", provider.id),
        subject: subject.to_owned(),
        display_name: display_name.to_owned(),
        grant,
    })
}

fn single_header<'a>(
    headers: &[(&str, &'a str)],
    name: &str,
    limit: usize,
) -> anyhow::Result<&'a str> {
    let mut values = headers
        .iter()
        .filter_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(*value));
    let value = values.next().context("missing identity proxy assertion")?;
    ensure!(
        values.next().is_none(),
        "duplicate identity proxy assertion"
    );
    ensure!(
        !value.is_empty()
            && value.len() <= limit
            && value.trim() == value
            && !value.contains(',')
            && !value.chars().any(char::is_control),
        "malformed identity proxy assertion"
    );
    Ok(value)
}

#[cfg(test)]
#[path = "proxy_identity_tests.rs"]
mod tests;
