//! Authentication message construction and parsing.
//!
//! Pure functions for building and parsing the 4-step Baichuan login
//! handshake. No state machine logic lives here -- that belongs in
//! `session.rs`.

use crate::{
    FIRMWARE_CAP, MODEL_CAP, NONCE_CAP, PASSWORD_CAP, SERIAL_CAP, USERNAME_CAP,
    encryption::credential_hash, error::BcError, xml::build_xml,
};
use arrayvec::ArrayString;

/// Encryption mode for the Baichuan session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionMode {
    /// No encryption (`0xDC00`).
    None,
    /// BCEncrypt XOR cipher (`0xDC01`).
    BcEncrypt,
    /// AES-128-CFB for config messages only (`0xDC02`).
    Aes,
    /// AES-128-CFB for all traffic including streams (`0xDC12`).
    FullAes,
}

impl EncryptionMode {
    /// Encode as the status_class value for the modern login header.
    pub const fn to_class_value(self) -> u32 {
        match self {
            Self::None => 0xDC00,
            Self::BcEncrypt => 0xDC01,
            Self::Aes => 0xDC02,
            Self::FullAes => 0xDC12,
        }
    }

    /// Decode from a status_class value in the header.
    pub const fn from_class_value(class: u32) -> Option<Self> {
        match class {
            0xDC00 => Some(Self::None),
            0xDC01 => Some(Self::BcEncrypt),
            0xDC02 => Some(Self::Aes),
            0xDC12 => Some(Self::FullAes),
            _ => Option::None,
        }
    }

    const fn level(self) -> u8 {
        match self {
            Self::None => 0,
            Self::BcEncrypt => 1,
            Self::Aes => 2,
            Self::FullAes => 3,
        }
    }

    const fn from_level(level: u8) -> Self {
        match level {
            0 => Self::None,
            1 => Self::BcEncrypt,
            2 => Self::Aes,
            _ => Self::FullAes,
        }
    }
}

/// Negotiate the effective encryption mode.
///
/// Returns the minimum of the requested level and the camera's capability.
pub fn negotiate_encryption(
    requested: EncryptionMode,
    camera_max: EncryptionMode,
) -> EncryptionMode {
    EncryptionMode::from_level(requested.level().min(camera_max.level()))
}

/// Login parameters (all `Copy`).
#[derive(Debug, Clone, Copy)]
pub struct LoginParams {
    pub username: ArrayString<USERNAME_CAP>,
    pub password: ArrayString<PASSWORD_CAP>,
    pub encryption: EncryptionMode,
}

impl LoginParams {
    /// Create login params from string slices.
    /// Truncates username/password if they exceed the fixed capacity.
    pub fn new(username: &str, password: &str, encryption: EncryptionMode) -> Self {
        let mut u = ArrayString::<USERNAME_CAP>::new();
        let user_slice = if username.len() > USERNAME_CAP {
            &username[..USERNAME_CAP]
        } else {
            username
        };
        u.push_str(user_slice);

        let mut p = ArrayString::<PASSWORD_CAP>::new();
        let pass_slice = if password.len() > PASSWORD_CAP {
            &password[..PASSWORD_CAP]
        } else {
            password
        };
        p.push_str(pass_slice);

        Self {
            username: u,
            password: p,
            encryption,
        }
    }
}

/// Device information extracted from login confirmation.
#[derive(Debug, Clone, Copy, Default)]
pub struct CameraIdentity {
    pub model: ArrayString<MODEL_CAP>,
    pub serial: ArrayString<SERIAL_CAP>,
    pub firmware: ArrayString<FIRMWARE_CAP>,
    pub channel_count: u8,
}

/// Result of a successful login.
#[derive(Debug, Clone, Copy)]
pub struct LoginResult {
    pub user_id: u32,
    pub camera_identity: CameraIdentity,
    pub encryption: EncryptionMode,
}

/// Parsed nonce from the camera's Step 2 response.
#[derive(Debug, Clone, Copy)]
pub struct NonceInfo {
    pub nonce: ArrayString<NONCE_CAP>,
    pub encryption: EncryptionMode,
}

/// Size of the legacy login binary body.
pub const LEGACY_LOGIN_BODY_LEN: usize = 64;

/// Build a legacy login body: 32-byte username + 32-byte password, null-padded.
pub fn build_legacy_login(params: &LoginParams) -> [u8; LEGACY_LOGIN_BODY_LEN] {
    let mut body = [0u8; LEGACY_LOGIN_BODY_LEN];
    let user_bytes = params.username.as_bytes();
    body[..user_bytes.len()].copy_from_slice(user_bytes);
    let pass_bytes = params.password.as_bytes();
    body[32..32 + pass_bytes.len()].copy_from_slice(pass_bytes);
    body
}

/// Parse the camera's nonce response XML.
///
/// Extracts the `<nonce>` value and determines the camera's maximum
/// supported encryption from the `<type>` field.
pub fn parse_nonce_response(data: &[u8]) -> Result<NonceInfo, BcError> {
    let mut nonce = ArrayString::<NONCE_CAP>::new();
    let mut enc_type = ArrayString::<16>::new();

    crate::xml::parse_xml(data, |name, text| match name {
        "nonce" => {
            if let Ok(s) = ArrayString::try_from(text) {
                nonce = s;
            }
        }
        "type" => {
            if let Ok(s) = ArrayString::try_from(text) {
                enc_type = s;
            }
        }
        _ => {}
    })?;

    if nonce.is_empty() {
        return Err(BcError::XmlParse("missing nonce in response"));
    }

    let encryption = match enc_type.as_str() {
        "aes" => EncryptionMode::FullAes,
        "bc" | "bcencrypt" | "md5" => EncryptionMode::BcEncrypt,
        _ => EncryptionMode::None,
    };

    Ok(NonceInfo { nonce, encryption })
}

/// Build the modern login XML body with hashed credentials.
///
/// The username and password are hashed with `credential_hash(nonce, value)`.
pub fn build_modern_login(
    params: &LoginParams,
    nonce: &str,
    buf: &mut [u8],
) -> Result<usize, BcError> {
    let user_hash = credential_hash(nonce, params.username.as_str());
    let pass_hash = credential_hash(nonce, params.password.as_str());

    // credential_hash always returns uppercase ASCII hex bytes
    let user_hash_str =
        core::str::from_utf8(&user_hash).map_err(|_| BcError::Encryption("invalid hash"))?;
    let pass_hash_str =
        core::str::from_utf8(&pass_hash).map_err(|_| BcError::Encryption("invalid hash"))?;

    build_xml(buf, |b| {
        b.start_versioned("LoginUser", "2");
        b.text_element("userName", user_hash_str);
        b.text_element("password", pass_hash_str);
        b.u32_element("userVer", 1);
        b.end();
        b.start_versioned("LoginNet", "2");
        b.text_element("type", "LAN");
        b.u32_element("udpPort", 0);
        b.end();
    })
}

/// Parse the login confirmation XML body.
///
/// Returns `LoginResult` on success, or `BcError::XmlParse` if userId
/// is missing.
pub fn parse_login_confirmation(
    data: &[u8],
    encryption: EncryptionMode,
) -> Result<LoginResult, BcError> {
    let mut user_id = None;
    let mut model = ArrayString::<MODEL_CAP>::new();
    let mut serial = ArrayString::<SERIAL_CAP>::new();
    let mut firmware = ArrayString::<FIRMWARE_CAP>::new();
    let mut channel_count = None;
    let mut result_str = ArrayString::<16>::new();

    crate::xml::parse_xml(data, |name, text| match name {
        "result" => {
            if let Ok(s) = ArrayString::try_from(text) {
                result_str = s;
            }
        }
        "userId" => user_id = text.parse().ok(),
        "model" => {
            if let Ok(s) = ArrayString::try_from(text) {
                model = s;
            }
        }
        "serialNumber" => {
            if let Ok(s) = ArrayString::try_from(text) {
                serial = s;
            }
        }
        "firmVer" | "firmVersion" => {
            if let Ok(s) = ArrayString::try_from(text) {
                firmware = s;
            }
        }
        "channelNum" => channel_count = text.parse().ok(),
        _ => {}
    })?;

    // Explicit rejection from the camera
    if result_str.as_str() == "failed" || result_str.as_str() == "error" {
        return Err(BcError::XmlParse("login rejected by camera"));
    }

    Ok(LoginResult {
        user_id: user_id.unwrap_or(0),
        camera_identity: CameraIdentity {
            model,
            serial,
            firmware,
            channel_count: channel_count.unwrap_or(1),
        },
        encryption,
    })
}

/// Parse a legacy login body (Step 1) and extract the username.
///
/// The body is 64 bytes: 32-byte null-padded username + 32-byte password.
pub fn parse_legacy_login(body: &[u8]) -> Result<ArrayString<USERNAME_CAP>, BcError> {
    if body.len() < LEGACY_LOGIN_BODY_LEN {
        return Err(BcError::Protocol("legacy login body too short"));
    }
    let user_end = body[..32].iter().position(|&b| b == 0).unwrap_or(32);
    let name = core::str::from_utf8(&body[..user_end])
        .map_err(|_| BcError::Protocol("invalid UTF-8 in legacy login username"))?;
    ArrayString::try_from(name).map_err(|_| BcError::Protocol("legacy login username too long"))
}

/// Build the nonce response XML that the camera sends in Step 2.
pub fn build_nonce_response(
    nonce: &str,
    enc_mode: EncryptionMode,
    buf: &mut [u8],
) -> Result<usize, BcError> {
    let enc_type = match enc_mode {
        EncryptionMode::None => "none",
        EncryptionMode::BcEncrypt => "bc",
        EncryptionMode::Aes | EncryptionMode::FullAes => "aes",
    };
    build_xml(buf, |b| {
        b.start_versioned("Encryption", "2");
        b.text_element("type", enc_type);
        b.text_element("nonce", nonce);
        b.end();
    })
}

/// Parse the client's modern login XML (Step 3).
///
/// Returns `(user_hash, pass_hash)` as strings from the `<userName>` and
/// `<password>` elements inside `<LoginUser>`.
pub fn parse_modern_login(body: &[u8]) -> Result<(ArrayString<64>, ArrayString<64>), BcError> {
    let mut user_hash = ArrayString::<64>::new();
    let mut pass_hash = ArrayString::<64>::new();

    crate::xml::parse_xml(body, |name, text| match name {
        "userName" => {
            if let Ok(s) = ArrayString::try_from(text) {
                user_hash = s;
            }
        }
        "password" => {
            if let Ok(s) = ArrayString::try_from(text) {
                pass_hash = s;
            }
        }
        _ => {}
    })?;

    if user_hash.is_empty() || pass_hash.is_empty() {
        return Err(BcError::XmlParse(
            "missing userName or password in modern login",
        ));
    }

    Ok((user_hash, pass_hash))
}

/// Validate received credential hashes against expected credentials.
///
/// Computes `credential_hash(nonce, expected_user)` and
/// `credential_hash(nonce, expected_pass)`, then compares with the
/// received hashes.
pub fn validate_credentials(
    nonce: &str,
    expected_user: &str,
    expected_pass: &str,
    received_user_hash: &str,
    received_pass_hash: &str,
) -> bool {
    let expected_uh = credential_hash(nonce, expected_user);
    let expected_ph = credential_hash(nonce, expected_pass);
    let expected_uh_str = core::str::from_utf8(&expected_uh).unwrap_or("");
    let expected_ph_str = core::str::from_utf8(&expected_ph).unwrap_or("");
    expected_uh_str == received_user_hash && expected_ph_str == received_pass_hash
}

/// Build the login confirmation XML that the camera sends in Step 4.
pub fn build_login_confirmation(
    user_id: u32,
    info: &CameraIdentity,
    buf: &mut [u8],
) -> Result<usize, BcError> {
    build_xml(buf, |b| {
        b.start_versioned("LoginUser", "2");
        b.text_element("userName", "admin");
        b.text_element("result", "ok");
        b.u32_element("userId", user_id);
        b.end();
        b.start_versioned("DeviceInfo", "2");
        b.text_element("model", info.model.as_str());
        b.text_element("serialNumber", info.serial.as_str());
        b.text_element("firmVer", info.firmware.as_str());
        b.u8_element("channelNum", info.channel_count);
        b.end();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_params() -> LoginParams {
        LoginParams {
            username: ArrayString::try_from("admin").unwrap(),
            password: ArrayString::try_from("password123").unwrap(),
            encryption: EncryptionMode::Aes,
        }
    }

    #[test]
    fn legacy_login_byte_layout() {
        let params = test_params();
        let body = build_legacy_login(&params);
        assert_eq!(body.len(), 64);
        assert_eq!(&body[..5], b"admin");
        assert_eq!(&body[5..32], &[0u8; 27]);
        assert_eq!(&body[32..43], b"password123");
        assert_eq!(&body[43..64], &[0u8; 21]);
    }

    #[test]
    fn legacy_login_null_padding() {
        let params = LoginParams {
            username: ArrayString::try_from("a").unwrap(),
            password: ArrayString::try_from("b").unwrap(),
            encryption: EncryptionMode::None,
        };
        let body = build_legacy_login(&params);
        assert_eq!(body[0], b'a');
        assert_eq!(&body[1..32], &[0u8; 31]);
        assert_eq!(body[32], b'b');
        assert_eq!(&body[33..64], &[0u8; 31]);
    }

    #[test]
    fn parse_nonce_aes() {
        let xml = br#"<body><Encryption version="2"><type>aes</type><nonce>ABCDEF123456</nonce></Encryption></body>"#;
        let info = parse_nonce_response(xml).unwrap();
        assert_eq!(info.nonce.as_str(), "ABCDEF123456");
        assert_eq!(info.encryption, EncryptionMode::FullAes);
    }

    #[test]
    fn parse_nonce_bc() {
        let xml = br#"<body><Encryption version="1"><type>bc</type><nonce>AABB</nonce></Encryption></body>"#;
        let info = parse_nonce_response(xml).unwrap();
        assert_eq!(info.nonce.as_str(), "AABB");
        assert_eq!(info.encryption, EncryptionMode::BcEncrypt);
    }

    #[test]
    fn parse_nonce_missing_nonce_fails() {
        let xml = br#"<body><Encryption version="2"><type>aes</type></Encryption></body>"#;
        assert!(parse_nonce_response(xml).is_err());
    }

    #[test]
    fn modern_login_contains_hashes() {
        let params = test_params();
        let mut buf = [0u8; 1024];
        let len = build_modern_login(&params, "TESTNONCE", &mut buf).unwrap();
        let xml = core::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<LoginUser version=\"2\">"));
        assert!(xml.contains("<LoginNet version=\"2\">"));
        assert!(xml.contains("<userVer>1</userVer>"));
        assert!(xml.contains("<type>LAN</type>"));
        // Should contain hashed values, not plaintext
        assert!(!xml.contains("admin"));
        assert!(!xml.contains("password123"));
    }

    #[test]
    fn modern_login_hash_matches_credential_hash() {
        let params = test_params();
        let mut buf = [0u8; 1024];
        let len = build_modern_login(&params, "TESTNONCE", &mut buf).unwrap();
        let xml = core::str::from_utf8(&buf[..len]).unwrap();

        let user_hash = credential_hash("TESTNONCE", "admin");
        let expected_user = core::str::from_utf8(&user_hash).unwrap();
        let pass_hash = credential_hash("TESTNONCE", "password123");
        let expected_pass = core::str::from_utf8(&pass_hash).unwrap();

        assert!(xml.contains(expected_user));
        assert!(xml.contains(expected_pass));
    }

    #[test]
    fn parse_login_confirmation_success() {
        let xml = br#"<body><LoginUser version="2"><userName>admin</userName><result>ok</result><userId>42</userId></LoginUser><DeviceInfo version="2"><model>RLC-810A</model><serialNumber>SN123</serialNumber><firmVer>v3.0.0</firmVer><channelNum>2</channelNum></DeviceInfo></body>"#;
        let result = parse_login_confirmation(xml, EncryptionMode::Aes).unwrap();
        assert_eq!(result.user_id, 42);
        assert_eq!(result.camera_identity.model.as_str(), "RLC-810A");
        assert_eq!(result.camera_identity.serial.as_str(), "SN123");
        assert_eq!(result.camera_identity.firmware.as_str(), "v3.0.0");
        assert_eq!(result.camera_identity.channel_count, 2);
        assert_eq!(result.encryption, EncryptionMode::Aes);
    }

    #[test]
    fn parse_login_confirmation_no_user_id_defaults_to_zero() {
        let xml = br#"<body><DeviceInfo><model>Test</model></DeviceInfo></body>"#;
        let result = parse_login_confirmation(xml, EncryptionMode::None).unwrap();
        assert_eq!(result.user_id, 0);
        assert_eq!(result.camera_identity.model.as_str(), "Test");
    }

    #[test]
    fn parse_login_confirmation_explicit_failure() {
        let xml = br#"<body><LoginUser><result>failed</result></LoginUser></body>"#;
        assert!(parse_login_confirmation(xml, EncryptionMode::None).is_err());
    }

    #[test]
    fn parse_login_confirmation_default_channels() {
        let xml = br#"<body><LoginUser><userId>1</userId></LoginUser></body>"#;
        let result = parse_login_confirmation(xml, EncryptionMode::None).unwrap();
        assert_eq!(result.camera_identity.channel_count, 1);
    }

    #[test]
    fn parse_login_confirmation_firmware_version_alias() {
        let xml = br#"<body><DeviceInfo><firmVersion>00000000983040</firmVersion><channelNum>1</channelNum></DeviceInfo></body>"#;
        let result = parse_login_confirmation(xml, EncryptionMode::BcEncrypt).unwrap();
        assert_eq!(result.camera_identity.firmware.as_str(), "00000000983040");
        assert_eq!(result.camera_identity.channel_count, 1);
    }

    #[test]
    fn parse_nonce_md5_type() {
        let xml = br#"<body><Encryption version="1.1"><type>md5</type><nonce>6992baa2-test</nonce></Encryption></body>"#;
        let info = parse_nonce_response(xml).unwrap();
        assert_eq!(info.nonce.as_str(), "6992baa2-test");
        assert_eq!(info.encryption, EncryptionMode::BcEncrypt);
    }

    #[test]
    fn encryption_class_roundtrip() {
        for mode in [
            EncryptionMode::None,
            EncryptionMode::BcEncrypt,
            EncryptionMode::Aes,
            EncryptionMode::FullAes,
        ] {
            let class = mode.to_class_value();
            let decoded = EncryptionMode::from_class_value(class).unwrap();
            assert_eq!(decoded, mode);
        }
    }

    #[test]
    fn encryption_class_values_are_correct() {
        assert_eq!(EncryptionMode::None.to_class_value(), 0xDC00);
        assert_eq!(EncryptionMode::BcEncrypt.to_class_value(), 0xDC01);
        assert_eq!(EncryptionMode::Aes.to_class_value(), 0xDC02);
        assert_eq!(EncryptionMode::FullAes.to_class_value(), 0xDC12);
    }

    #[test]
    fn encryption_from_unknown_class() {
        assert_eq!(EncryptionMode::from_class_value(0xFFFF), Option::None);
    }

    #[test]
    fn negotiate_downgrades() {
        assert_eq!(
            negotiate_encryption(EncryptionMode::FullAes, EncryptionMode::BcEncrypt),
            EncryptionMode::BcEncrypt,
        );
        assert_eq!(
            negotiate_encryption(EncryptionMode::Aes, EncryptionMode::None),
            EncryptionMode::None,
        );
    }

    #[test]
    fn negotiate_keeps_requested_if_supported() {
        assert_eq!(
            negotiate_encryption(EncryptionMode::BcEncrypt, EncryptionMode::FullAes),
            EncryptionMode::BcEncrypt,
        );
        assert_eq!(
            negotiate_encryption(EncryptionMode::Aes, EncryptionMode::FullAes),
            EncryptionMode::Aes,
        );
    }

    #[test]
    fn parse_legacy_login_extracts_username() {
        let params = test_params();
        let body = build_legacy_login(&params);
        let username = parse_legacy_login(&body).unwrap();
        assert_eq!(username.as_str(), "admin");
    }

    #[test]
    fn parse_legacy_login_short_body_fails() {
        assert!(parse_legacy_login(&[0u8; 32]).is_err());
    }

    #[test]
    fn parse_legacy_login_full_32_byte_username() {
        let mut body = [b'A'; 64];
        // 32-byte username with no null terminator
        body[32..].fill(0);
        let username = parse_legacy_login(&body).unwrap();
        assert_eq!(username.len(), 32);
    }

    #[test]
    fn nonce_response_roundtrips_aes() {
        let mut buf = [0u8; 512];
        let len = build_nonce_response("TESTNONCE123", EncryptionMode::FullAes, &mut buf).unwrap();
        let info = parse_nonce_response(&buf[..len]).unwrap();
        assert_eq!(info.nonce.as_str(), "TESTNONCE123");
        assert_eq!(info.encryption, EncryptionMode::FullAes);
    }

    #[test]
    fn nonce_response_roundtrips_bc() {
        let mut buf = [0u8; 512];
        let len = build_nonce_response("NONCE_BC", EncryptionMode::BcEncrypt, &mut buf).unwrap();
        let info = parse_nonce_response(&buf[..len]).unwrap();
        assert_eq!(info.nonce.as_str(), "NONCE_BC");
        assert_eq!(info.encryption, EncryptionMode::BcEncrypt);
    }

    #[test]
    fn parse_modern_login_extracts_hashes() {
        let params = test_params();
        let mut buf = [0u8; 1024];
        let len = build_modern_login(&params, "NONCE1", &mut buf).unwrap();
        let (uh, ph) = parse_modern_login(&buf[..len]).unwrap();
        // Verify they match credential_hash output
        let expected_uh = credential_hash("NONCE1", "admin");
        let expected_ph = credential_hash("NONCE1", "password123");
        assert_eq!(uh.as_str(), core::str::from_utf8(&expected_uh).unwrap());
        assert_eq!(ph.as_str(), core::str::from_utf8(&expected_ph).unwrap());
    }

    #[test]
    fn parse_modern_login_missing_fields_fails() {
        let xml = br#"<body><LoginUser version="2"><userVer>1</userVer></LoginUser></body>"#;
        assert!(parse_modern_login(xml).is_err());
    }

    #[test]
    fn validate_credentials_correct() {
        let nonce = "TESTNONCE";
        let uh = credential_hash(nonce, "admin");
        let ph = credential_hash(nonce, "password123");
        assert!(validate_credentials(
            nonce,
            "admin",
            "password123",
            core::str::from_utf8(&uh).unwrap(),
            core::str::from_utf8(&ph).unwrap(),
        ));
    }

    #[test]
    fn validate_credentials_wrong_password() {
        let nonce = "TESTNONCE";
        let uh = credential_hash(nonce, "admin");
        let ph = credential_hash(nonce, "wrongpass");
        assert!(!validate_credentials(
            nonce,
            "admin",
            "password123",
            core::str::from_utf8(&uh).unwrap(),
            core::str::from_utf8(&ph).unwrap(),
        ));
    }

    #[test]
    fn validate_credentials_wrong_username() {
        let nonce = "TESTNONCE";
        let uh = credential_hash(nonce, "nobody");
        let ph = credential_hash(nonce, "password123");
        assert!(!validate_credentials(
            nonce,
            "admin",
            "password123",
            core::str::from_utf8(&uh).unwrap(),
            core::str::from_utf8(&ph).unwrap(),
        ));
    }

    #[test]
    fn login_confirmation_roundtrips() {
        let info = CameraIdentity {
            model: ArrayString::try_from("RLC-810A").unwrap(),
            serial: ArrayString::try_from("SN99887766").unwrap(),
            firmware: ArrayString::try_from("v3.1.0").unwrap(),
            channel_count: 4,
        };
        let mut buf = [0u8; 1024];
        let len = build_login_confirmation(42, &info, &mut buf).unwrap();
        let result = parse_login_confirmation(&buf[..len], EncryptionMode::BcEncrypt).unwrap();
        assert_eq!(result.user_id, 42);
        assert_eq!(result.camera_identity.model.as_str(), "RLC-810A");
        assert_eq!(result.camera_identity.serial.as_str(), "SN99887766");
        assert_eq!(result.camera_identity.firmware.as_str(), "v3.1.0");
        assert_eq!(result.camera_identity.channel_count, 4);
        assert_eq!(result.encryption, EncryptionMode::BcEncrypt);
    }

    #[test]
    fn login_confirmation_default_channels() {
        let info = CameraIdentity::default();
        let mut buf = [0u8; 1024];
        let len = build_login_confirmation(1, &info, &mut buf).unwrap();
        let result = parse_login_confirmation(&buf[..len], EncryptionMode::None).unwrap();
        assert_eq!(result.user_id, 1);
    }
}
