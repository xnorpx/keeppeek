use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha512};
use std::{error::Error as StdError, fmt};

const SETUP_URI_PREFIX: &str = "X-HM://";
const SETUP_PAYLOAD_WIDTH: usize = 9;
const SUPPORTS_IP_FLAG: u64 = 1 << 28;

/// HAP accessory category advertised during discovery and encoded in setup data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AccessoryCategory {
    Bridge = 2,
    IpCamera = 17,
}

/// Stable HAP accessory identifier rendered like a MAC address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessoryId([u8; 6]);

impl AccessoryId {
    pub const fn new(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for AccessoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02X}:{b:02X}:{c:02X}:{d:02X}:{e:02X}:{g:02X}")
    }
}

/// Eight-digit HomeKit setup code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupCode(u32);

impl SetupCode {
    /// Parses the canonical `XXX-XX-XXX` representation.
    pub fn parse(value: &str) -> Result<Self, SetupError> {
        let bytes = value.as_bytes();
        if bytes.len() != 10
            || bytes[3] != b'-'
            || bytes[6] != b'-'
            || bytes
                .iter()
                .enumerate()
                .any(|(index, byte)| !matches!(index, 3 | 6) && !byte.is_ascii_digit())
        {
            return Err(SetupError::InvalidCode);
        }
        let digits = bytes
            .iter()
            .filter(|byte| byte.is_ascii_digit())
            .fold(0_u32, |value, byte| value * 10 + u32::from(*byte - b'0'));
        if matches!(
            digits,
            12_345_678
                | 87_654_321
                | 0
                | 11_111_111
                | 22_222_222
                | 33_333_333
                | 44_444_444
                | 55_555_555
                | 66_666_666
                | 77_777_777
                | 88_888_888
                | 99_999_999
        ) {
            return Err(SetupError::UnsafeCode);
        }
        Ok(Self(digits))
    }
}

impl fmt::Display for SetupCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = format!("{:08}", self.0);
        write!(f, "{}-{}-{}", &value[..3], &value[3..5], &value[5..])
    }
}

/// Four-character setup identifier appended to a HomeKit setup URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupId(String);

impl SetupId {
    pub fn parse(value: &str) -> Result<Self, SetupError> {
        if value.len() != 4
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(SetupError::InvalidSetupId);
        }
        Ok(Self(value.to_owned()))
    }
}

/// Whether the accessory is available for initial Pair Setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BonjourStatus {
    Paired = 0,
    NotPaired = 1,
}

/// Stable setup material used by QR generation and HAP discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupPayload {
    code: SetupCode,
    setup_id: SetupId,
    accessory_id: AccessoryId,
    category: AccessoryCategory,
}

impl SetupPayload {
    pub const fn new(
        code: SetupCode,
        setup_id: SetupId,
        accessory_id: AccessoryId,
        category: AccessoryCategory,
    ) -> Self {
        Self {
            code,
            setup_id,
            accessory_id,
            category,
        }
    }

    pub const fn code(&self) -> SetupCode {
        self.code
    }

    /// Returns the stable identifier advertised by the accessory.
    pub const fn accessory_id(&self) -> AccessoryId {
        self.accessory_id
    }

    /// Returns the payload encoded by a HomeKit setup QR code.
    pub fn uri(&self) -> String {
        let payload =
            u64::from(self.code.0) | SUPPORTS_IP_FLAG | (u64::from(self.category as u8) << 31);
        format!(
            "{SETUP_URI_PREFIX}{payload:0>width$}{}",
            self.setup_id.0,
            payload = base36(payload),
            width = SETUP_PAYLOAD_WIDTH,
        )
    }

    /// Computes the Bonjour `sh` value used to match a scanned setup QR code.
    pub fn setup_hash(&self) -> String {
        let digest = Sha512::digest(format!("{}{}", self.setup_id.0, self.accessory_id));
        STANDARD.encode(&digest[..4])
    }

    /// Returns the HAP Bonjour TXT records for an IP accessory.
    pub fn bonjour_txt(
        &self,
        name: &str,
        configuration_number: u32,
        status: BonjourStatus,
    ) -> Vec<String> {
        vec![
            format!("c#={configuration_number}"),
            "ff=0".to_owned(),
            format!("id={}", self.accessory_id),
            format!("md={name}"),
            "pv=1.1".to_owned(),
            "s#=1".to_owned(),
            format!("sf={}", status as u8),
            format!("ci={}", self.category as u8),
            format!("sh={}", self.setup_hash()),
        ]
    }
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut encoded = [b'0'; 13];
    let mut offset = encoded.len();
    while value != 0 {
        offset -= 1;
        encoded[offset] = DIGITS[(value % 36) as usize];
        value /= 36;
    }
    String::from_utf8(encoded[offset..].to_vec()).expect("base36 is ASCII")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupError {
    InvalidCode,
    UnsafeCode,
    InvalidSetupId,
}

impl fmt::Display for SetupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCode => f.write_str("setup code must use XXX-XX-XXX digits"),
            Self::UnsafeCode => f.write_str("setup code is prohibited by HAP"),
            Self::InvalidSetupId => {
                f.write_str("setup identifier must contain four uppercase letters or digits")
            }
        }
    }
}

impl StdError for SetupError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_homekit_setup_uri_vector() {
        let payload = SetupPayload::new(
            SetupCode::parse("031-45-154").unwrap(),
            SetupId::parse("7U1H").unwrap(),
            AccessoryId::new([0xCC, 0x22, 0x3D, 0xE3, 0xCE, 0xF6]),
            AccessoryCategory::Bridge,
        );

        assert_eq!(payload.uri(), "X-HM://0023ISYWY7U1H");
        assert_eq!(payload.setup_hash(), "KW9mHw==");
        assert_eq!(
            payload.bonjour_txt("KeepPeek", 1, BonjourStatus::NotPaired),
            [
                "c#=1",
                "ff=0",
                "id=CC:22:3D:E3:CE:F6",
                "md=KeepPeek",
                "pv=1.1",
                "s#=1",
                "sf=1",
                "ci=2",
                "sh=KW9mHw==",
            ]
        );
    }

    #[test]
    fn rejects_invalid_and_unsafe_setup_material() {
        assert_eq!(SetupCode::parse("03145154"), Err(SetupError::InvalidCode));
        assert_eq!(SetupCode::parse("123-45-678"), Err(SetupError::UnsafeCode));
        assert_eq!(SetupId::parse("abcd"), Err(SetupError::InvalidSetupId));
    }
}
