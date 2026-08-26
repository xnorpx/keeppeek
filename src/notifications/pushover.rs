use serde::{Deserialize, Serialize};
use url::Url;

const KEY_LENGTH: usize = 30;
const MAX_DEVICE_LENGTH: usize = 25;
const MAX_SOUND_LENGTH: usize = 64;
const MIN_EMERGENCY_RETRY_SECONDS: u16 = 30;
const MAX_EMERGENCY_EXPIRE_SECONDS: u16 = 10_800;
const MAX_TITLE_CHARS: usize = 250;
const MAX_MESSAGE_CHARS: usize = 1_024;
const MAX_DEEP_LINK_CHARS: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Destination {
    pub application_token: String,
    pub user_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound: Option<String>,
    #[serde(default)]
    pub priority: i8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_seconds: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_seconds: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep_link_base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicConfig {
    pub device: Option<String>,
    pub sound: Option<String>,
    #[serde(default)]
    pub priority: i8,
    pub retry_seconds: Option<u16>,
    pub expire_seconds: Option<u16>,
    pub deep_link_base_url: Option<String>,
}

impl Destination {
    pub(super) fn parse(value: &str) -> anyhow::Result<Self> {
        let destination: Self = serde_json::from_str(value)
            .map_err(|_| anyhow::anyhow!("Pushover destination must be valid JSON"))?;
        destination.validate()?;
        Ok(destination)
    }

    pub(super) fn deep_link(&self, value: &str) -> anyhow::Result<Option<String>> {
        if value.is_empty() {
            return Ok(None);
        }
        if let Ok(url) = Url::parse(value) {
            return validated_deep_link(url).map(Some);
        }
        let Some(base_url) = &self.deep_link_base_url else {
            return Ok(None);
        };
        let url = Url::parse(base_url)?.join(value)?;
        validated_deep_link(url).map(Some)
    }

    fn validate(&self) -> anyhow::Result<()> {
        validate_key(&self.application_token, "Pushover application token")?;
        validate_key(&self.user_key, "Pushover user or group key")?;
        if let Some(device) = &self.device
            && (device.is_empty()
                || device.split(',').any(|name| {
                    name.is_empty()
                        || name.len() > MAX_DEVICE_LENGTH
                        || !name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                }))
        {
            anyhow::bail!(
                "Pushover devices must be comma-separated names of 1 to {MAX_DEVICE_LENGTH} letters, digits, '_' or '-'"
            );
        }
        if let Some(sound) = &self.sound
            && (sound.is_empty()
                || sound.len() > MAX_SOUND_LENGTH
                || !sound
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
        {
            anyhow::bail!(
                "Pushover sound must contain 1 to {MAX_SOUND_LENGTH} letters, digits, '_' or '-'"
            );
        }
        if !(-2..=2).contains(&self.priority) {
            anyhow::bail!("Pushover priority must be between -2 and 2");
        }
        if self.priority == 2 {
            if self
                .retry_seconds
                .is_none_or(|retry| retry < MIN_EMERGENCY_RETRY_SECONDS)
            {
                anyhow::bail!("Pushover emergency retry must be at least 30 seconds");
            }
            if self
                .expire_seconds
                .is_none_or(|expire| expire == 0 || expire > MAX_EMERGENCY_EXPIRE_SECONDS)
            {
                anyhow::bail!("Pushover emergency expiry must be between 1 and 10800 seconds");
            }
        } else if self.retry_seconds.is_some() || self.expire_seconds.is_some() {
            anyhow::bail!("Pushover retry and expiry are only valid for emergency priority");
        }
        if let Some(base_url) = &self.deep_link_base_url {
            let parsed = Url::parse(base_url)
                .map_err(|_| anyhow::anyhow!("Pushover deep-link base must be an absolute URL"))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || !parsed.username().is_empty()
                || parsed.password().is_some()
            {
                anyhow::bail!("Pushover deep-link base must be an HTTP(S) URL without credentials");
            }
        }
        Ok(())
    }
}

pub(super) fn validate_template(title: &str, message: &str) -> anyhow::Result<()> {
    if title.chars().count() > MAX_TITLE_CHARS {
        anyhow::bail!("Pushover title exceeds {MAX_TITLE_CHARS} characters");
    }
    if message.chars().count() > MAX_MESSAGE_CHARS {
        anyhow::bail!("Pushover message exceeds {MAX_MESSAGE_CHARS} characters");
    }
    Ok(())
}

pub fn public_config(value: &str) -> anyhow::Result<PublicConfig> {
    let destination = Destination::parse(value)?;
    Ok(PublicConfig {
        device: destination.device,
        sound: destination.sound,
        priority: destination.priority,
        retry_seconds: destination.retry_seconds,
        expire_seconds: destination.expire_seconds,
        deep_link_base_url: destination.deep_link_base_url,
    })
}

pub fn merge_public_config(
    value: &str,
    public_config: serde_json::Value,
) -> anyhow::Result<String> {
    let mut destination = Destination::parse(value)?;
    let public_config: PublicConfig = serde_json::from_value(public_config)
        .map_err(|_| anyhow::anyhow!("Pushover public configuration is invalid"))?;
    destination.device = public_config.device;
    destination.sound = public_config.sound;
    destination.priority = public_config.priority;
    destination.retry_seconds = public_config.retry_seconds;
    destination.expire_seconds = public_config.expire_seconds;
    destination.deep_link_base_url = public_config.deep_link_base_url;
    destination.validate()?;
    serde_json::to_string(&destination).map_err(Into::into)
}

fn validate_key(value: &str, name: &str) -> anyhow::Result<()> {
    if value.len() != KEY_LENGTH || !value.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        anyhow::bail!("{name} must contain exactly {KEY_LENGTH} ASCII letters or digits");
    }
    Ok(())
}

fn validate_http_url(url: Url) -> anyhow::Result<Url> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        anyhow::bail!("Pushover deep link must be an HTTP(S) URL without credentials");
    }
    Ok(url)
}

fn validated_deep_link(url: Url) -> anyhow::Result<String> {
    let url = validate_http_url(url)?.to_string();
    if url.chars().count() > MAX_DEEP_LINK_CHARS {
        anyhow::bail!("Pushover deep link exceeds {MAX_DEEP_LINK_CHARS} characters");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination(priority: i8) -> Destination {
        Destination {
            application_token: "a23456789012345678901234567890".to_owned(),
            user_key: "u23456789012345678901234567890".to_owned(),
            device: Some("phone,tablet-2".to_owned()),
            sound: Some("spacealarm".to_owned()),
            priority,
            retry_seconds: (priority == 2).then_some(30),
            expire_seconds: (priority == 2).then_some(300),
            deep_link_base_url: Some("https://keeppeek.example/viewer/".to_owned()),
        }
    }

    #[test]
    fn validates_normal_and_emergency_destinations() {
        destination(0).validate().unwrap();
        destination(2).validate().unwrap();
    }

    #[test]
    fn rejects_invalid_credentials_and_emergency_bounds() {
        let mut invalid_key = destination(0);
        invalid_key.user_key = "secret".to_owned();
        assert!(invalid_key.validate().is_err());

        let mut invalid_emergency = destination(2);
        invalid_emergency.retry_seconds = Some(29);
        invalid_emergency.expire_seconds = Some(10_801);
        assert!(invalid_emergency.validate().is_err());

        let mut invalid_normal = destination(0);
        invalid_normal.retry_seconds = Some(30);
        assert!(invalid_normal.validate().is_err());
    }

    #[test]
    fn public_configuration_round_trip_does_not_expose_credentials() {
        let configured = destination(0);
        let serialized = serde_json::to_string(
            &public_config(&serde_json::to_string(&configured).unwrap()).unwrap(),
        )
        .unwrap();
        assert!(!serialized.contains(&configured.application_token));
        assert!(!serialized.contains(&configured.user_key));

        let mut public: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        public["priority"] = 2.into();
        public["retry_seconds"] = 60.into();
        public["expire_seconds"] = 600.into();
        let merged =
            merge_public_config(&serde_json::to_string(&configured).unwrap(), public).unwrap();
        let merged = Destination::parse(&merged).unwrap();
        assert_eq!(merged.application_token, configured.application_token);
        assert_eq!(merged.user_key, configured.user_key);
        assert_eq!(merged.priority, 2);
        assert_eq!(merged.retry_seconds, Some(60));
        assert_eq!(merged.expire_seconds, Some(600));
    }

    #[test]
    fn enforces_provider_message_limits() {
        validate_template(&"t".repeat(250), &"m".repeat(1_024)).unwrap();
        assert!(validate_template(&"t".repeat(251), "message").is_err());
        assert!(validate_template("title", &"m".repeat(1_025)).is_err());
    }
}
