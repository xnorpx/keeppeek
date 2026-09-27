//! Checks whether a candidate keeps an already usable remote Administrator path.

use super::{AccessRole, PersistedAccessCatalog, StoredCredential, identities::Directory};
use crate::config::Config;
use anyhow::Result;

pub fn has_current_administrator(config: &Config, now_ms: i64) -> Result<bool> {
    let bearer_enabled = config.external_auth.as_ref().is_none_or(|settings| {
        settings.bearer_enabled
            && settings
                .bearer_transition_until_ms
                .is_some_and(|until| now_ms < until)
    });
    if bearer_enabled {
        let catalog = credentials(config)?;
        if catalog.iter().any(|credential| {
            credential.role == AccessRole::Administrator
                && !credential.initial_secret_pending
                && credential.is_active(credential.revision, now_ms)
                && selected_by_bearer(&catalog, credential)
        }) {
            return Ok(true);
        }
    }
    // Missing or stale admission evidence must not disable the lockout guard itself.
    Ok(config.external_auth.is_some()
        && Directory::from_root(&config.source)?
            .records
            .iter()
            .any(|identity| identity.enabled && identity.role == AccessRole::Administrator))
}

pub fn preserves_administrator(before: &Config, after: &Config, now_ms: i64) -> Result<bool> {
    if !same_transport(before, after) {
        return Ok(false);
    }
    let bearer_was_enabled = before.external_auth.as_ref().is_none_or(|settings| {
        settings.bearer_enabled
            && settings
                .bearer_transition_until_ms
                .is_some_and(|until| now_ms < until)
    });
    if bearer_was_enabled && after.external_auth.is_none() {
        let previous = credentials(before)?;
        let next = credentials(after)?;
        if next.iter().any(|candidate| {
            durable_bearer(candidate, now_ms)
                && selected_by_bearer(&next, candidate)
                && previous.iter().any(|existing| {
                    durable_bearer(existing, now_ms)
                        && candidate.id == existing.id
                        && candidate.verifier == existing.verifier
                        && selected_by_bearer(&previous, existing)
                })
        }) {
            return Ok(true);
        }
    }
    preserves_external_administrator(before, after)
}

pub fn same_transport(before: &Config, after: &Config) -> bool {
    before.host == after.host
        && before.port == after.port
        && before.access.local_networks == after.access.local_networks
        && before.access.trusted_proxies == after.access.trusted_proxies
        && before.access.require_secure_remote == after.access.require_secure_remote
}

pub fn verify_replacement_bearer(
    config: &Config,
    key: &str,
    now_ms: i64,
) -> Result<(uuid::Uuid, u64)> {
    anyhow::ensure!(
        config.external_auth.is_none() && key.len() <= 64,
        "replacement bearer is unavailable"
    );
    let key = super::AccessKey::parse(key)
        .map_err(|_| anyhow::anyhow!("replacement bearer is invalid"))?;
    anyhow::ensure!(!key.is_unset(), "replacement bearer is invalid");
    let catalog = credentials(config)?;
    let selected = super::matching_credential_index(&catalog, key.fingerprint())
        .and_then(|index| catalog.get(index))
        .ok_or_else(|| anyhow::anyhow!("replacement bearer is invalid"))?;
    anyhow::ensure!(
        durable_bearer(selected, now_ms),
        "replacement requires a permanent claimed Administrator"
    );
    Ok((selected.id, selected.revision))
}

pub fn verified_bearer_is_active(
    config: &Config,
    binding: (uuid::Uuid, u64),
    now_ms: i64,
) -> Result<bool> {
    if config.external_auth.is_some() {
        return Ok(false);
    }
    let catalog = credentials(config)?;
    let Some(candidate) = catalog.iter().find(|credential| credential.id == binding.0) else {
        return Ok(false);
    };
    Ok(candidate.revision == binding.1
        && durable_bearer(candidate, now_ms)
        && selected_by_bearer(&catalog, candidate))
}

fn selected_by_bearer(catalog: &[StoredCredential], candidate: &StoredCredential) -> bool {
    // ponytail: at most 128 credentials bound these repeated scans; index only if profiling requires it.
    super::matching_credential_index(catalog, candidate.verifier)
        .is_some_and(|index| catalog[index].id == candidate.id)
}

fn credentials(config: &Config) -> Result<Vec<StoredCredential>> {
    super::validate_configuration(&config.source)?;
    config
        .source
        .get(super::ACCESS_CATALOG_SECTION)
        .map_or_else(
            || Ok(Vec::new()),
            |value| {
                Ok(value
                    .clone()
                    .try_into::<PersistedAccessCatalog>()?
                    .credentials)
            },
        )
}

fn durable_bearer(credential: &StoredCredential, now_ms: i64) -> bool {
    credential.role == AccessRole::Administrator
        && !credential.initial_secret_pending
        && credential.expires_at_ms.is_none()
        && credential.is_active(credential.revision, now_ms)
}

fn preserves_external_administrator(before: &Config, after: &Config) -> Result<bool> {
    let (Some(previous), Some(next)) = (&before.external_auth, &after.external_auth) else {
        return Ok(false);
    };
    if previous.allowed_origins != next.allowed_origins {
        return Ok(false);
    }
    let previous_directory = Directory::from_root(&before.source)?;
    let next_directory = Directory::from_root(&after.source)?;
    let policies = next
        .providers
        .iter()
        .map(|provider| {
            Ok((
                &provider.id,
                super::external::admission_policy_fingerprint(provider)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(next_directory.records.iter().any(|candidate| {
        candidate.enabled
            && candidate.role == AccessRole::Administrator
            && previous_directory.records.iter().any(|existing| {
                existing.enabled
                    && existing.role == AccessRole::Administrator
                    && existing.id == candidate.id
                    && existing.subject_fingerprint == candidate.subject_fingerprint
                    && existing.provider_id == candidate.provider_id
                    && existing.admission_policy_fingerprint
                        == candidate.admission_policy_fingerprint
            })
            && next.providers.iter().any(|provider| {
                provider.id == candidate.provider_id
                    && policies.iter().any(|(id, fingerprint)| {
                        *id == &provider.id
                            && candidate.admission_policy_fingerprint.as_ref() == Some(fingerprint)
                    })
                    && provider
                        .mappings
                        .iter()
                        .any(|mapping| mapping.role == AccessRole::Administrator)
                    && previous.providers.contains(provider)
            })
    }))
}

#[cfg(test)]
mod tests {
    use crate::access::{AccessKey, PersistedAccessCatalog, legacy_credential};
    use crate::config::Config;

    fn bearer_configuration() -> Config {
        let mut credential = legacy_credential(AccessKey::generate(), 1);
        credential.initial_secret_pending = false;
        let catalog = PersistedAccessCatalog {
            version: 1,
            credentials: vec![credential],
        };
        let mut config = Config::default();
        config.source.insert(
            "access_credentials".into(),
            toml::Value::try_from(catalog).unwrap(),
        );
        config
    }

    #[test]
    fn replacement_bearer_proof_requires_a_permanent_claimed_administrator() {
        let key = AccessKey::generate();
        let mut credential = legacy_credential(key, 1);
        credential.initial_secret_pending = false;
        let id = credential.id;
        let mut candidate = Config::default();
        candidate.source.insert(
            "access_credentials".into(),
            toml::Value::try_from(PersistedAccessCatalog {
                version: 1,
                credentials: vec![credential],
            })
            .unwrap(),
        );
        assert_eq!(
            super::verify_replacement_bearer(&candidate, &key.canonical(), 2).unwrap(),
            (id, 1)
        );
        assert!(
            super::verify_replacement_bearer(&candidate, &AccessKey::generate().canonical(), 2)
                .is_err()
        );
        for (field, value) in [
            ("disabled", toml::Value::Boolean(true)),
            ("initial_secret_pending", toml::Value::Boolean(true)),
            ("expires_at_ms", toml::Value::Integer(10_000)),
            ("revoked_at_ms", toml::Value::Integer(1)),
            ("role", toml::Value::String("user".into())),
        ] {
            let mut denied = candidate.clone();
            denied.source["access_credentials"]["credentials"][0]
                .as_table_mut()
                .unwrap()
                .insert(field.into(), value);
            assert!(
                super::verify_replacement_bearer(&denied, &key.canonical(), 2).is_err(),
                "{field}"
            );
        }
        candidate.external_auth = external_configuration().external_auth;
        assert!(super::verify_replacement_bearer(&candidate, &key.canonical(), 2).is_err());
        assert!(super::verify_replacement_bearer(&Config::default(), "0", 2).is_err());
    }

    #[test]
    fn replacement_bearer_proof_matches_normal_duplicate_verifier_selection() {
        let key = AccessKey::generate();
        let mut administrator = legacy_credential(key, 1);
        administrator.initial_secret_pending = false;
        let mut last = administrator.clone();
        last.id = uuid::Uuid::new_v4();
        last.role = super::AccessRole::User;
        let mut candidate = Config::default();
        candidate.source.insert(
            "access_credentials".into(),
            toml::Value::try_from(PersistedAccessCatalog {
                version: 1,
                credentials: vec![administrator, last],
            })
            .unwrap(),
        );
        assert!(super::verify_replacement_bearer(&candidate, &key.canonical(), 2).is_err());
        candidate.source["access_credentials"]["credentials"][1]["role"] = "administrator".into();
        candidate.source["access_credentials"]["credentials"][1]["disabled"] = true.into();
        assert!(super::verify_replacement_bearer(&candidate, &key.canonical(), 2).is_err());
    }

    #[test]
    fn retained_bearer_requires_the_same_nonexpiring_usable_credential_and_transport() {
        let before = bearer_configuration();
        let mut after = before.clone();
        assert!(super::preserves_administrator(&before, &after, 2).unwrap());
        after.source["access_credentials"]["credentials"][0]
            .as_table_mut()
            .unwrap()
            .insert("expires_at_ms".into(), 10_000.into());
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
        assert!(super::has_current_administrator(&after, 2).unwrap());
        assert!(!super::has_current_administrator(&after, 10_000).unwrap());
        after = before.clone();
        after.source["access_credentials"]["credentials"][0]["disabled"] = true.into();
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
        after = bearer_configuration();
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
        after = before.clone();
        after
            .access
            .trusted_proxies
            .push("203.0.113.1/32".parse().unwrap());
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
    }

    fn external_configuration() -> Config {
        use crate::access::{
            AccessRole, CameraAccess,
            external::Grant,
            identities::{Directory, IdentityInput},
        };
        let mut config = Config {
            external_auth: Some(
                toml::from_str(
                    r#"
allowed_origins = ["https://keeppeek.example"]
[[providers]]
id = "company"
name = "Company"
mappings = [{claim = "role", value = "admins", role = "administrator"}]
[providers.method]
kind = "proxy"
trusted_peers = ["203.0.113.1/32"]
subject_header = "X-Identity-Subject"
role_header = "X-Identity-Role"
"#,
                )
                .unwrap(),
            ),
            ..Config::default()
        };
        let mut directory = Directory::default();
        directory
            .provision_authenticated(
                IdentityInput {
                    provider_id: "company",
                    namespace: "proxy:company",
                    subject: "alice",
                    display_name: "Alice",
                    grant: Grant {
                        role: AccessRole::Administrator,
                        camera_access: CameraAccess::unrestricted(),
                    },
                    now_ms: 1,
                },
                &config.external_auth.as_ref().unwrap().providers[0],
            )
            .unwrap();
        config.source.insert(
            "external_identities".into(),
            toml::Value::try_from(directory).unwrap(),
        );
        config
    }

    #[test]
    fn changed_mappings_do_not_make_historical_administrators_retained_replacements() {
        let mut config = external_configuration();
        config.external_auth.as_mut().unwrap().providers[0].mappings[0].value = "new-admins".into();
        let persisted = toml::to_string(&config.source).unwrap();
        config.source = toml::from_str(&persisted).unwrap();
        assert!(super::has_current_administrator(&config, 2).unwrap());
        assert!(!super::preserves_administrator(&config, &config, 2).unwrap());
    }

    #[test]
    fn legacy_administrator_records_require_reauthentication_before_retention() {
        let mut config = external_configuration();
        config.source["external_identities"]["records"][0]
            .as_table_mut()
            .unwrap()
            .remove("admission_policy_fingerprint");
        assert!(super::has_current_administrator(&config, 2).unwrap());
        assert!(!super::preserves_administrator(&config, &config, 2).unwrap());
    }

    #[test]
    fn revoking_new_administrator_cannot_fall_back_to_a_stale_policy_grant() {
        use crate::access::{
            AccessRole, CameraAccess,
            external::Grant,
            identities::{Directory, IdentityInput},
        };
        let mut config = external_configuration();
        config.external_auth.as_mut().unwrap().providers[0].mappings[0].value = "new-admins".into();
        let mut directory = Directory::from_root(&config.source).unwrap();
        let bob = directory
            .provision_authenticated(
                IdentityInput {
                    provider_id: "company",
                    namespace: "proxy:company",
                    subject: "bob",
                    display_name: "Bob",
                    grant: Grant {
                        role: AccessRole::Administrator,
                        camera_access: CameraAccess::unrestricted(),
                    },
                    now_ms: 2,
                },
                &config.external_auth.as_ref().unwrap().providers[0],
            )
            .unwrap();
        config.source.insert(
            "external_identities".into(),
            toml::Value::try_from(&directory).unwrap(),
        );
        let serialized = toml::to_string(&config.source).unwrap();
        config.source = toml::from_str(&serialized).unwrap();
        assert!(super::preserves_administrator(&config, &config, 3).unwrap());
        let mut after = config.clone();
        let mut reloaded = Directory::from_root(&after.source).unwrap();
        reloaded.revoke(bob.id).unwrap();
        after.source.insert(
            "external_identities".into(),
            toml::Value::try_from(reloaded).unwrap(),
        );
        assert!(super::has_current_administrator(&config, 3).unwrap());
        assert!(!super::preserves_administrator(&config, &after, 3).unwrap());
    }

    #[test]
    fn external_admin_preservation_checks_origins_mappings_identity_and_future_bearer_cutoff() {
        let before = external_configuration();
        let mut after = before.clone();
        assert!(super::preserves_administrator(&before, &after, 2).unwrap());
        after.external_auth.as_mut().unwrap().allowed_origins =
            vec!["https://different.example".into()];
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
        after = before.clone();
        after.external_auth.as_mut().unwrap().providers[0].mappings[0].value = "different".into();
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
        after = before.clone();
        after.source["external_identities"]["records"][0]["enabled"] = false.into();
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
        let mut before = bearer_configuration();
        after = before.clone();
        after.external_auth = external_configuration().external_auth;
        let settings = after.external_auth.as_mut().unwrap();
        settings.bearer_enabled = true;
        settings.bearer_transition_until_ms = Some(10_000);
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
        before.source["access_credentials"]["credentials"][0]["initial_secret_pending"] =
            true.into();
        after = before.clone();
        assert!(!super::preserves_administrator(&before, &after, 2).unwrap());
    }
}
