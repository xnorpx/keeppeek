//! SRTP `AES_CM_128_HMAC_SHA1_80` protection for the legacy HomeKit camera path.
//!
//! HomeKit hands the accessory a master key and salt through Setup Endpoints
//! rather than negotiating them over DTLS, so this implements the RFC 3711 key
//! derivation and transform directly. Only the sender direction is provided;
//! the accessory never decrypts controller media on this path.

use aes::Aes128;
use aes::cipher::{KeyIvInit, StreamCipher};
use ctr::Ctr128BE;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use std::{error::Error as StdError, fmt};

type Aes128Ctr = Ctr128BE<Aes128>;
type HmacSha1 = Hmac<Sha1>;

/// Length of the truncated HMAC-SHA1 tag appended to every packet.
pub const AUTH_TAG_LEN: usize = 10;

const RTP_FIXED_HEADER_LEN: usize = 12;
const LABEL_ENCRYPTION: u8 = 0;
const LABEL_AUTHENTICATION: u8 = 1;
const LABEL_SALT: u8 = 2;
const LABEL_RTCP_ENCRYPTION: u8 = 3;
const LABEL_RTCP_AUTHENTICATION: u8 = 4;
const LABEL_RTCP_SALT: u8 = 5;
const SRTCP_ENCRYPTED: u32 = 1 << 31;

/// Failure while protecting an outgoing RTP packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrtpError {
    /// The buffer is shorter than the RTP header it claims to carry.
    ShortPacket,
    /// The packet does not start with an RTP version 2 header.
    NotRtp,
    /// The packet does not carry a valid RTCP packet type.
    NotRtcp,
}

impl fmt::Display for SrtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShortPacket => f.write_str("packet is shorter than its RTP header"),
            Self::NotRtp => f.write_str("packet is not RTP version 2"),
            Self::NotRtcp => f.write_str("packet is not RTCP"),
        }
    }
}

/// Sender-side SRTCP context for one synchronization source.
pub struct SrtcpSession {
    session_key: [u8; 16],
    session_salt: [u8; 14],
    auth_key: [u8; 20],
    index: u32,
}

impl fmt::Debug for SrtcpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SrtcpSession")
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

impl SrtcpSession {
    /// Derives SRTCP keys from the HomeKit master key and salt.
    pub fn new(master_key: &[u8; 16], master_salt: &[u8; 14]) -> Self {
        let mut session_key = [0_u8; 16];
        let mut auth_key = [0_u8; 20];
        let mut session_salt = [0_u8; 14];
        derive(
            master_key,
            master_salt,
            LABEL_RTCP_ENCRYPTION,
            &mut session_key,
        );
        derive(
            master_key,
            master_salt,
            LABEL_RTCP_AUTHENTICATION,
            &mut auth_key,
        );
        derive(master_key, master_salt, LABEL_RTCP_SALT, &mut session_salt);
        Self {
            session_key,
            session_salt,
            auth_key,
            index: 1,
        }
    }

    /// Encrypts an RTCP compound packet and appends its index and authentication tag.
    pub fn protect(&mut self, packet: &mut Vec<u8>) -> Result<(), SrtpError> {
        if packet.len() < 8 {
            return Err(SrtpError::ShortPacket);
        }
        if packet[0] >> 6 != 2 || !(192..=223).contains(&packet[1]) {
            return Err(SrtpError::NotRtcp);
        }
        let ssrc = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        let index = self.index & !SRTCP_ENCRYPTED;
        let iv = counter_iv(&self.session_salt, ssrc, u64::from(index));
        let mut cipher = Aes128Ctr::new(&self.session_key.into(), &iv.into());
        cipher.apply_keystream(&mut packet[8..]);

        packet.extend_from_slice(&(index | SRTCP_ENCRYPTED).to_be_bytes());
        let mut mac =
            HmacSha1::new_from_slice(&self.auth_key).expect("HMAC accepts keys of any length");
        mac.update(packet);
        packet.extend_from_slice(&mac.finalize().into_bytes()[..AUTH_TAG_LEN]);
        self.index = self.index.wrapping_add(1) & !SRTCP_ENCRYPTED;
        Ok(())
    }
}

impl StdError for SrtpError {}

/// Sender-side SRTP context for one synchronization source.
pub struct SrtpSession {
    session_key: [u8; 16],
    session_salt: [u8; 14],
    auth_key: [u8; 20],
    roc: u32,
    last_sequence: Option<u16>,
}

impl fmt::Debug for SrtpSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SrtpSession")
            .field("roc", &self.roc)
            .field("last_sequence", &self.last_sequence)
            .finish_non_exhaustive()
    }
}

impl SrtpSession {
    /// Derives session keys from the master key and salt supplied by the controller.
    pub fn new(master_key: &[u8; 16], master_salt: &[u8; 14]) -> Self {
        let mut session_key = [0_u8; 16];
        let mut auth_key = [0_u8; 20];
        let mut session_salt = [0_u8; 14];
        derive(master_key, master_salt, LABEL_ENCRYPTION, &mut session_key);
        derive(master_key, master_salt, LABEL_AUTHENTICATION, &mut auth_key);
        derive(master_key, master_salt, LABEL_SALT, &mut session_salt);
        Self {
            session_key,
            session_salt,
            auth_key,
            roc: 0,
            last_sequence: None,
        }
    }

    /// Encrypts the payload in place and appends the authentication tag.
    pub fn protect(&mut self, packet: &mut Vec<u8>) -> Result<(), SrtpError> {
        if packet.len() < RTP_FIXED_HEADER_LEN {
            return Err(SrtpError::ShortPacket);
        }
        if packet[0] >> 6 != 2 {
            return Err(SrtpError::NotRtp);
        }
        let header_len = rtp_header_len(packet)?;
        let sequence = u16::from_be_bytes([packet[2], packet[3]]);
        let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
        let roc = self.advance(sequence);
        let index = (u64::from(roc) << 16) | u64::from(sequence);

        let iv = counter_iv(&self.session_salt, ssrc, index);
        let mut cipher = Aes128Ctr::new(&self.session_key.into(), &iv.into());
        cipher.apply_keystream(&mut packet[header_len..]);

        let mut mac =
            HmacSha1::new_from_slice(&self.auth_key).expect("HMAC accepts keys of any length");
        mac.update(packet);
        mac.update(&roc.to_be_bytes());
        packet.extend_from_slice(&mac.finalize().into_bytes()[..AUTH_TAG_LEN]);
        Ok(())
    }

    /// Tracks the rollover counter across the 16-bit sequence number wrap.
    const fn advance(&mut self, sequence: u16) -> u32 {
        if let Some(previous) = self.last_sequence
            && sequence < previous
        {
            self.roc = self.roc.wrapping_add(1);
        }
        self.last_sequence = Some(sequence);
        self.roc
    }
}

/// Returns the RTP header length including CSRCs and any extension.
fn rtp_header_len(packet: &[u8]) -> Result<usize, SrtpError> {
    let csrc_count = usize::from(packet[0] & 0x0f);
    let mut length = RTP_FIXED_HEADER_LEN + csrc_count * 4;
    if packet[0] & 0x10 != 0 {
        if packet.len() < length + 4 {
            return Err(SrtpError::ShortPacket);
        }
        let words = usize::from(u16::from_be_bytes([packet[length + 2], packet[length + 3]]));
        length += 4 + words * 4;
    }
    if packet.len() < length {
        return Err(SrtpError::ShortPacket);
    }
    Ok(length)
}

/// RFC 3711 section 4.3.1 key derivation with a key derivation rate of zero.
fn derive(master_key: &[u8; 16], master_salt: &[u8; 14], label: u8, output: &mut [u8]) {
    let mut iv = [0_u8; 16];
    iv[..14].copy_from_slice(master_salt);
    // key_id is `label || index_div_kdr`, left-padded to the salt width, which
    // places the label byte at offset 7.
    iv[7] ^= label;
    output.fill(0);
    Aes128Ctr::new(master_key.into(), &iv.into()).apply_keystream(output);
}

/// RFC 3711 section 4.1.1 counter block for one packet.
fn counter_iv(salt: &[u8; 14], ssrc: u32, index: u64) -> [u8; 16] {
    let mut iv = [0_u8; 16];
    iv[..14].copy_from_slice(salt);
    for (offset, byte) in ssrc.to_be_bytes().iter().enumerate() {
        iv[4 + offset] ^= byte;
    }
    for (offset, byte) in index.to_be_bytes()[2..].iter().enumerate() {
        iv[8 + offset] ^= byte;
    }
    iv
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER_KEY: [u8; 16] = [
        0xE1, 0xF9, 0x7A, 0x0D, 0x3E, 0x01, 0x8B, 0xE0, 0xD6, 0x4F, 0xA3, 0x2C, 0x06, 0xDE, 0x41,
        0x39,
    ];
    const MASTER_SALT: [u8; 14] = [
        0x0E, 0xC6, 0x75, 0xAD, 0x49, 0x8A, 0xFE, 0xEB, 0xB6, 0x96, 0x0B, 0x3A, 0xAB, 0xE6,
    ];

    #[test]
    fn derives_rfc3711_appendix_b2_session_keys() {
        let mut cipher_key = [0_u8; 16];
        let mut salt = [0_u8; 14];
        let mut auth_key = [0_u8; 20];
        derive(&MASTER_KEY, &MASTER_SALT, LABEL_ENCRYPTION, &mut cipher_key);
        derive(&MASTER_KEY, &MASTER_SALT, LABEL_SALT, &mut salt);
        derive(
            &MASTER_KEY,
            &MASTER_SALT,
            LABEL_AUTHENTICATION,
            &mut auth_key,
        );

        assert_eq!(
            cipher_key,
            [
                0xC6, 0x1E, 0x7A, 0x93, 0x74, 0x4F, 0x39, 0xEE, 0x10, 0x73, 0x4A, 0xFE, 0x3F, 0xF7,
                0xA0, 0x87
            ]
        );
        assert_eq!(
            salt,
            [
                0x30, 0xCB, 0xBC, 0x08, 0x86, 0x3D, 0x8C, 0x85, 0xD4, 0x9D, 0xB3, 0x4A, 0x9A, 0xE1
            ]
        );
        assert_eq!(
            auth_key,
            [
                0xCE, 0xBE, 0x32, 0x1F, 0x6F, 0xF7, 0x71, 0x6B, 0x6F, 0xD4, 0xAB, 0x49, 0xAF, 0x25,
                0x6A, 0x15, 0x6D, 0x38, 0xBA, 0xA4
            ]
        );
    }

    #[test]
    fn matches_the_libsrtp_aes_cm_128_hmac_sha1_80_vector() {
        // libsrtp `srtp_driver.c` reference packet: PT 15, seq 0x1234,
        // timestamp 0xdecafbad, SSRC 0xcafebabe, sixteen 0xab payload bytes.
        let mut packet = vec![
            0x80, 0x0f, 0x12, 0x34, 0xde, 0xca, 0xfb, 0xad, 0xca, 0xfe, 0xba, 0xbe,
        ];
        packet.extend_from_slice(&[0xab; 16]);

        let mut session = SrtpSession::new(&MASTER_KEY, &MASTER_SALT);
        session.protect(&mut packet).unwrap();

        let expected = [
            0x80, 0x0f, 0x12, 0x34, 0xde, 0xca, 0xfb, 0xad, 0xca, 0xfe, 0xba, 0xbe, 0x4e, 0x55,
            0xdc, 0x4c, 0xe7, 0x99, 0x78, 0xd8, 0x8c, 0xa4, 0xd2, 0x15, 0x94, 0x9d, 0x24, 0x02,
            0xb7, 0x8d, 0x6a, 0xcc, 0x99, 0xea, 0x17, 0x9b, 0x8d, 0xbb,
        ];
        assert_eq!(packet, expected);
    }

    fn packet(sequence: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x80, 0x60];
        out.extend_from_slice(&sequence.to_be_bytes());
        out.extend_from_slice(&0_u32.to_be_bytes());
        out.extend_from_slice(&0xDEAD_BEEF_u32.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn appends_a_ten_byte_tag_and_encrypts_only_the_payload() {
        let mut session = SrtpSession::new(&MASTER_KEY, &MASTER_SALT);
        let plaintext = [1_u8, 2, 3, 4, 5, 6, 7, 8];
        let mut protected = packet(1, &plaintext);
        let header = protected[..RTP_FIXED_HEADER_LEN].to_vec();
        session.protect(&mut protected).unwrap();

        assert_eq!(
            protected.len(),
            RTP_FIXED_HEADER_LEN + plaintext.len() + AUTH_TAG_LEN
        );
        assert_eq!(&protected[..RTP_FIXED_HEADER_LEN], &header[..]);
        assert_ne!(
            &protected[RTP_FIXED_HEADER_LEN..RTP_FIXED_HEADER_LEN + plaintext.len()],
            &plaintext[..]
        );
    }

    #[test]
    fn identical_payloads_differ_per_sequence_number() {
        let mut session = SrtpSession::new(&MASTER_KEY, &MASTER_SALT);
        let mut first = packet(1, &[0; 16]);
        let mut second = packet(2, &[0; 16]);
        session.protect(&mut first).unwrap();
        session.protect(&mut second).unwrap();
        assert_ne!(
            first[RTP_FIXED_HEADER_LEN..],
            second[RTP_FIXED_HEADER_LEN..]
        );
    }

    #[test]
    fn rolls_over_the_counter_when_the_sequence_wraps() {
        let mut session = SrtpSession::new(&MASTER_KEY, &MASTER_SALT);
        session.protect(&mut packet(65_535, &[0; 4])).unwrap();
        assert_eq!(session.roc, 0);
        session.protect(&mut packet(0, &[0; 4])).unwrap();
        assert_eq!(session.roc, 1);
    }

    #[test]
    fn skips_csrc_and_extension_headers_when_encrypting() {
        let mut session = SrtpSession::new(&MASTER_KEY, &MASTER_SALT);
        // One CSRC and a one-word extension.
        let mut out = vec![0x91, 0x60, 0, 7];
        out.extend_from_slice(&0_u32.to_be_bytes());
        out.extend_from_slice(&0xDEAD_BEEF_u32.to_be_bytes());
        out.extend_from_slice(&0xAAAA_AAAA_u32.to_be_bytes());
        out.extend_from_slice(&[0xBE, 0xDE, 0x00, 0x01, 0x10, 0x20, 0x30, 0x40]);
        let header_len = out.len();
        out.extend_from_slice(&[9_u8; 4]);
        let header = out[..header_len].to_vec();
        session.protect(&mut out).unwrap();
        assert_eq!(&out[..header_len], &header[..]);
        assert_eq!(out.len(), header_len + 4 + AUTH_TAG_LEN);
    }

    #[test]
    fn rejects_non_rtp_and_truncated_buffers() {
        let mut session = SrtpSession::new(&MASTER_KEY, &MASTER_SALT);
        assert_eq!(
            session.protect(&mut vec![0; 4]),
            Err(SrtpError::ShortPacket)
        );
        let mut not_rtp = packet(1, &[0; 4]);
        not_rtp[0] = 0x40;
        assert_eq!(session.protect(&mut not_rtp), Err(SrtpError::NotRtp));
    }

    #[test]
    fn protects_rtcp_payload_and_appends_encrypted_index() {
        let mut session = SrtcpSession::new(&MASTER_KEY, &MASTER_SALT);
        let mut report = vec![0x80, 200, 0, 6];
        report.extend_from_slice(&0xDEAD_BEEF_u32.to_be_bytes());
        report.extend_from_slice(&[0x11; 20]);
        let header = report[..8].to_vec();

        session.protect(&mut report).unwrap();

        assert_eq!(&report[..8], &header);
        assert_ne!(&report[8..28], &[0x11; 20]);
        assert_eq!(&report[28..32], &(SRTCP_ENCRYPTED | 1).to_be_bytes());
        assert_eq!(report.len(), 28 + 4 + AUTH_TAG_LEN);
    }

    #[test]
    fn increments_srtcp_index_and_rejects_rtp() {
        let mut session = SrtcpSession::new(&MASTER_KEY, &MASTER_SALT);
        let mut first = vec![0x80, 200, 0, 1, 0, 0, 0, 1];
        let mut second = first.clone();
        session.protect(&mut first).unwrap();
        session.protect(&mut second).unwrap();
        assert_eq!(&first[8..12], &(SRTCP_ENCRYPTED | 1).to_be_bytes());
        assert_eq!(&second[8..12], &(SRTCP_ENCRYPTED | 2).to_be_bytes());

        let mut rtp = packet(1, &[0; 4]);
        assert_eq!(session.protect(&mut rtp), Err(SrtpError::NotRtcp));
    }

    #[test]
    fn matches_libsrtp_aes_128_packet_vectors() {
        let mut rtp = vec![
            0x80, 0x0f, 0x12, 0x34, 0xde, 0xca, 0xfb, 0xad, 0xca, 0xfe, 0xba, 0xbe, 0xab, 0xab,
            0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
        ];
        let expected_rtp = [
            0x80, 0x0f, 0x12, 0x34, 0xde, 0xca, 0xfb, 0xad, 0xca, 0xfe, 0xba, 0xbe, 0x4e, 0x55,
            0xdc, 0x4c, 0xe7, 0x99, 0x78, 0xd8, 0x8c, 0xa4, 0xd2, 0x15, 0x94, 0x9d, 0x24, 0x02,
            0xb7, 0x8d, 0x6a, 0xcc, 0x99, 0xea, 0x17, 0x9b, 0x8d, 0xbb,
        ];
        SrtpSession::new(&MASTER_KEY, &MASTER_SALT)
            .protect(&mut rtp)
            .unwrap();
        assert_eq!(rtp, expected_rtp);

        let mut rtcp = vec![
            0x81, 0xc8, 0x00, 0x0b, 0xca, 0xfe, 0xba, 0xbe, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
            0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab,
        ];
        let expected_rtcp = [
            0x81, 0xc8, 0x00, 0x0b, 0xca, 0xfe, 0xba, 0xbe, 0x71, 0x28, 0x03, 0x5b, 0xe4, 0x87,
            0xb9, 0xbd, 0xbe, 0xf8, 0x90, 0x41, 0xf9, 0x77, 0xa5, 0xa8, 0x80, 0x00, 0x00, 0x01,
            0x99, 0x3e, 0x08, 0xcd, 0x54, 0xd6, 0xc1, 0x23, 0x07, 0x98,
        ];
        SrtcpSession::new(&MASTER_KEY, &MASTER_SALT)
            .protect(&mut rtcp)
            .unwrap();
        assert_eq!(rtcp, expected_rtcp);
    }
}
