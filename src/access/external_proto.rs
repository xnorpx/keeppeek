//! Converts the typed administration contract without resolving stored secret references.

use super::external;
use super::{AccessRole, CameraAccess};
use crate::api::proto;
use anyhow::{Context, Result};

pub fn from_root(root: &toml::Table) -> Result<Option<proto::ExternalAuthenticationSettings>> {
    external::validate_source(root)?;
    root.get("external_auth")
        .map(|value| {
            let config: external::Config = value
                .clone()
                .try_into()
                .context("invalid external authentication settings")?;
            config.validate()?;
            Ok(encode(&config))
        })
        .transpose()
}

fn encode(config: &external::Config) -> proto::ExternalAuthenticationSettings {
    proto::ExternalAuthenticationSettings {
        allowed_origins: config.allowed_origins.clone(),
        providers: config.providers.iter().map(encode_provider).collect(),
        bearer_enabled: config.bearer_enabled,
        bearer_transition_until_ms: config.bearer_transition_until_ms,
    }
}

fn encode_provider(provider: &external::Provider) -> proto::ExternalAuthenticationProvider {
    use proto::external_authentication_provider::Method;
    let method = match &provider.method {
        external::Method::Oidc(config) => Method::Oidc(proto::OidcAuthenticationSettings {
            issuer: config.issuer.clone(),
            client_id: config.client_id.clone(),
            client_secret_reference: config.client_secret.clone(),
            redirect_uri: config.redirect_uri.clone(),
            scopes: config.scopes.clone(),
            display_name_claim: config.display_name_claim.clone(),
            endpoint_origins: config.endpoint_origins.clone(),
            private_networks: config
                .private_networks
                .iter()
                .map(ToString::to_string)
                .collect(),
            logout_uri: config.logout_uri.clone(),
        }),
        external::Method::Proxy(config) => Method::Proxy(proto::ProxyAuthenticationSettings {
            trusted_peers: config
                .trusted_peers
                .iter()
                .map(ToString::to_string)
                .collect(),
            subject_header: config.subject_header.clone(),
            role_header: config.role_header.clone(),
            name_header: config.name_header.clone(),
            secret_header: config.secret_header.clone(),
            shared_secret_reference: config.shared_secret.clone(),
        }),
    };
    proto::ExternalAuthenticationProvider {
        provider_id: provider.id.clone(),
        name: provider.name.clone(),
        method: Some(method),
        mappings: provider
            .mappings
            .iter()
            .map(|mapping| proto::ExternalRoleMapping {
                claim: mapping.claim.clone(),
                value: mapping.value.clone(),
                role: match mapping.role {
                    AccessRole::Administrator => proto::AccessRole::Administrator,
                    AccessRole::User => proto::AccessRole::User,
                } as i32,
                camera_access: mapping.camera_access.as_ref().map(|policy| {
                    proto::CameraAccessPolicy {
                        all_cameras: policy.all_cameras,
                        camera_ids: policy.camera_ids.clone(),
                        group_ids: policy.group_ids.clone(),
                    }
                }),
            })
            .collect(),
    }
}

pub fn decode(settings: proto::ExternalAuthenticationSettings) -> Result<external::Config> {
    let config = external::Config {
        allowed_origins: settings.allowed_origins,
        providers: settings
            .providers
            .into_iter()
            .map(decode_provider)
            .collect::<Result<_>>()?,
        bearer_enabled: settings.bearer_enabled,
        bearer_transition_until_ms: settings.bearer_transition_until_ms,
    };
    config.validate()?;
    let mut root = toml::Table::new();
    root.insert("external_auth".into(), toml::Value::try_from(&config)?);
    external::validate_source(&root)?;
    Ok(config)
}

fn decode_provider(provider: proto::ExternalAuthenticationProvider) -> Result<external::Provider> {
    use proto::external_authentication_provider::Method;
    let method = match provider.method.context("provider method is required")? {
        Method::Oidc(config) => external::Method::Oidc(external::Oidc {
            issuer: config.issuer,
            client_id: config.client_id,
            client_secret: config.client_secret_reference,
            redirect_uri: config.redirect_uri,
            scopes: config.scopes,
            display_name_claim: config.display_name_claim,
            endpoint_origins: config.endpoint_origins,
            private_networks: config
                .private_networks
                .iter()
                .map(|network| network.parse())
                .collect::<std::result::Result<_, _>>()
                .context("invalid provider network")?,
            logout_uri: config.logout_uri,
        }),
        Method::Proxy(config) => external::Method::Proxy(external::Proxy {
            trusted_peers: config
                .trusted_peers
                .iter()
                .map(|network| network.parse())
                .collect::<std::result::Result<_, _>>()
                .context("invalid proxy peer network")?,
            subject_header: config.subject_header,
            role_header: config.role_header,
            name_header: config.name_header,
            secret_header: config.secret_header,
            shared_secret: config.shared_secret_reference,
        }),
    };
    Ok(external::Provider {
        id: provider.provider_id,
        name: provider.name,
        method,
        mappings: provider
            .mappings
            .into_iter()
            .map(decode_mapping)
            .collect::<Result<_>>()?,
    })
}

fn decode_mapping(mapping: proto::ExternalRoleMapping) -> Result<external::Mapping> {
    let role = match proto::AccessRole::try_from(mapping.role) {
        Ok(proto::AccessRole::Administrator) => AccessRole::Administrator,
        Ok(proto::AccessRole::User) => AccessRole::User,
        _ => anyhow::bail!("mapping role must be Administrator or User"),
    };
    Ok(external::Mapping {
        claim: mapping.claim,
        value: mapping.value,
        role,
        camera_access: mapping.camera_access.map(|policy| CameraAccess {
            all_cameras: policy.all_cameras,
            camera_ids: policy.camera_ids,
            group_ids: policy.group_ids,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_preserves_secret_references_and_explicit_grants() {
        let config: external::Config = toml::from_str(
            r#"
allowed_origins = ["https://keeppeek.example"]
[[providers]]
id = "company"
name = "Company"
[[providers.mappings]]
claim = "groups"
value = "viewers"
role = "user"
[providers.mappings.camera_access]
all_cameras = false
camera_ids = ["front"]
[providers.method]
kind = "oidc"
issuer = "https://identity.example"
client_id = "keeppeek"
client_secret = "{secret:OIDC_SECRET}"
redirect_uri = "https://keeppeek.example/auth/callback"
"#,
        )
        .unwrap();
        let mut root = toml::Table::new();
        root.insert(
            "external_auth".into(),
            toml::Value::try_from(&config).unwrap(),
        );
        let wire = from_root(&root).unwrap().unwrap();
        let restored = decode(wire.clone()).unwrap();
        assert_eq!(
            toml::Value::try_from(config).unwrap(),
            toml::Value::try_from(restored).unwrap()
        );
        let mut invalid = wire;
        invalid.providers[0].mappings[0].role = 999;
        assert!(decode(invalid).is_err());
    }
}
