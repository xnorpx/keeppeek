use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use sha2::{Digest, Sha256};
use std::fmt;
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct AccessKey(u128);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessKeyFingerprint([u8; 32]);

impl AccessKeyFingerprint {
    pub fn matches(self, other: Self) -> bool {
        bool::from(self.0.ct_eq(&other.0))
    }
}

impl AccessKey {
    pub const fn unset() -> Self {
        Self(0)
    }

    pub const fn is_unset(self) -> bool {
        self.0 == 0
    }

    pub fn generate() -> Self {
        loop {
            let key = Self(Uuid::new_v4().as_u128());
            if !key.is_unset() {
                return key;
            }
        }
    }

    pub fn parse(value: &str) -> Result<Self, uuid::Error> {
        if value == "0" {
            return Ok(Self::unset());
        }
        Uuid::parse_str(value).map(|uuid| Self(uuid.as_u128()))
    }

    pub fn canonical(self) -> String {
        Uuid::from_u128(self.0).hyphenated().to_string()
    }

    pub fn fingerprint(self) -> AccessKeyFingerprint {
        AccessKeyFingerprint(Sha256::digest(self.0.to_be_bytes()).into())
    }
}

impl fmt::Debug for AccessKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessKey([redacted])")
    }
}

impl Serialize for AccessKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.is_unset() {
            serializer.serialize_u64(0)
        } else {
            serializer.serialize_str(&self.canonical())
        }
    }
}

impl<'de> Deserialize<'de> for AccessKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AccessKeyVisitor;

        impl de::Visitor<'_> for AccessKeyVisitor {
            type Value = AccessKey;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a hyphenated UUID string or the integer 0")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                AccessKey::parse(value).map_err(E::custom)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == 0 {
                    Ok(AccessKey::unset())
                } else {
                    Err(E::custom("integer access keys must be 0"))
                }
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value == 0 {
                    Ok(AccessKey::unset())
                } else {
                    Err(E::custom("integer access keys must be 0"))
                }
            }
        }

        deserializer.deserialize_any(AccessKeyVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_key_accepts_uuid_and_reserved_zero() {
        let key: AccessKey = toml::from_str("value = '550e8400-e29b-41d4-a716-446655440000'")
            .and_then(|table: toml::Table| table["value"].clone().try_into())
            .unwrap();
        assert_eq!(key.canonical(), "550e8400-e29b-41d4-a716-446655440000");

        let unset: AccessKey = toml::Value::Integer(0).try_into().unwrap();
        assert!(unset.is_unset());
        assert_eq!(
            toml::Value::try_from(unset).unwrap(),
            toml::Value::Integer(0)
        );
    }

    #[test]
    fn access_key_debug_output_is_redacted() {
        let key = AccessKey::parse("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(format!("{key:?}"), "AccessKey([redacted])");
        assert_ne!(key.fingerprint(), AccessKey::generate().fingerprint());
    }
}
