use num_bigint_dig::BigUint;
use num_traits::Zero;
use sha2::{Digest, Sha512};
use std::{error::Error as StdError, fmt};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const MODULUS_BYTES: &[u8; 384] = include_bytes!("srp_3072.bin");
const GENERATOR: u8 = 5;
const USERNAME: &[u8] = b"Pair-Setup";

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SrpServer {
    modulus: BigUint,
    generator: BigUint,
    salt: [u8; 16],
    verifier: BigUint,
    private: BigUint,
    public: BigUint,
}

impl SrpServer {
    pub fn new(
        password: &[u8],
        salt: [u8; 16],
        mut private_bytes: [u8; 32],
    ) -> Result<Self, SrpError> {
        if bool::from(private_bytes.ct_eq(&[0; 32])) {
            return Err(SrpError::InvalidPrivateKey);
        }
        let modulus = BigUint::from_bytes_be(MODULUS_BYTES);
        let generator = BigUint::from(GENERATOR);
        let private = BigUint::from_bytes_be(&private_bytes);
        private_bytes.zeroize();
        let x = compute_x(&salt, password);
        let verifier = generator.modpow(&x, &modulus);
        let multiplier = compute_multiplier(&modulus, &generator);
        let public = ((&multiplier * &verifier) % &modulus + generator.modpow(&private, &modulus))
            % &modulus;
        Ok(Self {
            modulus,
            generator,
            salt,
            verifier,
            private,
            public,
        })
    }

    pub const fn salt(&self) -> &[u8; 16] {
        &self.salt
    }

    pub fn public_key(&self) -> Vec<u8> {
        pad(&self.public)
    }

    pub fn verify(
        &self,
        controller_public_bytes: &[u8],
        controller_proof: &[u8],
    ) -> Result<SrpVerified, SrpError> {
        if controller_public_bytes.is_empty() || controller_public_bytes.len() > MODULUS_BYTES.len()
        {
            return Err(SrpError::InvalidControllerPublicKey);
        }
        let controller_public = BigUint::from_bytes_be(controller_public_bytes);
        if (&controller_public % &self.modulus).is_zero() {
            return Err(SrpError::InvalidControllerPublicKey);
        }
        let scrambler = compute_scrambler(&controller_public, &self.public);
        if scrambler.is_zero() {
            return Err(SrpError::InvalidScrambler);
        }
        let verifier_power = self.verifier.modpow(&scrambler, &self.modulus);
        let premaster_base = (&controller_public * verifier_power) % &self.modulus;
        let premaster = premaster_base.modpow(&self.private, &self.modulus);
        let session_key = Sha512::digest(pad(&premaster)).to_vec();
        let expected = proof_m1(
            &self.modulus,
            &self.generator,
            &self.salt,
            &controller_public,
            &self.public,
            &session_key,
        );
        if controller_proof.len() != expected.len()
            || !bool::from(controller_proof.ct_eq(&expected))
        {
            return Err(SrpError::ProofMismatch);
        }
        let proof = proof_m2(&controller_public, controller_proof, &session_key);
        Ok(SrpVerified {
            proof,
            session_key: Zeroizing::new(session_key),
        })
    }
}

pub struct SrpVerified {
    pub proof: Vec<u8>,
    pub session_key: Zeroizing<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SrpError {
    InvalidPrivateKey,
    InvalidControllerPublicKey,
    InvalidScrambler,
    ProofMismatch,
}

impl fmt::Display for SrpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPrivateKey => f.write_str("SRP private exponent is zero"),
            Self::InvalidControllerPublicKey => f.write_str("SRP controller public key is invalid"),
            Self::InvalidScrambler => f.write_str("SRP scrambling parameter is zero"),
            Self::ProofMismatch => f.write_str("SRP controller proof does not match"),
        }
    }
}

impl StdError for SrpError {}

fn compute_x(salt: &[u8], password: &[u8]) -> BigUint {
    let mut identity = Sha512::new();
    identity.update(USERNAME);
    identity.update(b":");
    identity.update(password);
    let identity = identity.finalize();
    let mut private = Sha512::new();
    private.update(salt);
    private.update(identity);
    BigUint::from_bytes_be(&private.finalize())
}

fn compute_multiplier(modulus: &BigUint, generator: &BigUint) -> BigUint {
    let mut hash = Sha512::new();
    hash.update(modulus.to_bytes_be());
    hash.update(pad(generator));
    BigUint::from_bytes_be(&hash.finalize())
}

fn compute_scrambler(controller_public: &BigUint, accessory_public: &BigUint) -> BigUint {
    let mut hash = Sha512::new();
    hash.update(pad(controller_public));
    hash.update(pad(accessory_public));
    BigUint::from_bytes_be(&hash.finalize())
}

fn proof_m1(
    modulus: &BigUint,
    generator: &BigUint,
    salt: &[u8],
    controller_public: &BigUint,
    accessory_public: &BigUint,
    session_key: &[u8],
) -> Vec<u8> {
    let modulus_hash = Sha512::digest(modulus.to_bytes_be());
    let generator_hash = Sha512::digest(generator.to_bytes_be());
    let xor_hash: Vec<u8> = modulus_hash
        .iter()
        .zip(generator_hash.iter())
        .map(|(left, right)| left ^ right)
        .collect();
    let username_hash = Sha512::digest(USERNAME);
    let mut proof = Sha512::new();
    proof.update(xor_hash);
    proof.update(username_hash);
    proof.update(salt);
    proof.update(pad(controller_public));
    proof.update(pad(accessory_public));
    proof.update(session_key);
    proof.finalize().to_vec()
}

fn proof_m2(controller_public: &BigUint, controller_proof: &[u8], session_key: &[u8]) -> Vec<u8> {
    let mut proof = Sha512::new();
    proof.update(pad(controller_public));
    proof.update(controller_proof);
    proof.update(session_key);
    proof.finalize().to_vec()
}

fn pad(value: &BigUint) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    if bytes.len() >= MODULUS_BYTES.len() {
        return bytes;
    }
    let mut padded = vec![0; MODULUS_BYTES.len() - bytes.len()];
    padded.extend_from_slice(&bytes);
    padded
}

#[cfg(test)]
pub fn test_controller_exchange(
    password: &[u8],
    salt: &[u8],
    private: &[u8],
    accessory_public_bytes: &[u8],
) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let modulus = BigUint::from_bytes_be(MODULUS_BYTES);
    let generator = BigUint::from(GENERATOR);
    let private = BigUint::from_bytes_be(private);
    let controller_public = generator.modpow(&private, &modulus);
    let accessory_public = BigUint::from_bytes_be(accessory_public_bytes);
    let multiplier = compute_multiplier(&modulus, &generator);
    let x = compute_x(salt, password);
    let scrambler = compute_scrambler(&controller_public, &accessory_public);
    let generator_x = generator.modpow(&x, &modulus);
    let multiplied = (&multiplier * generator_x) % &modulus;
    let base = (&accessory_public + &modulus - multiplied) % &modulus;
    let exponent = &private + &scrambler * x;
    let premaster = base.modpow(&exponent, &modulus);
    let session_key = Sha512::digest(pad(&premaster)).to_vec();
    let proof = proof_m1(
        &modulus,
        &generator,
        salt,
        &controller_public,
        &accessory_public,
        &session_key,
    );
    (pad(&controller_public), proof, session_key)
}

#[cfg(test)]
pub fn test_accessory_proof(
    controller_public: &[u8],
    controller_proof: &[u8],
    session_key: &[u8],
) -> Vec<u8> {
    proof_m2(
        &BigUint::from_bytes_be(controller_public),
        controller_proof,
        session_key,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_and_controller_agree_on_session_and_proofs() {
        let server = SrpServer::new(b"123-45-678", [0x11; 16], [0x5a; 32]).unwrap();
        let (controller_public, controller_proof, expected_key) = test_controller_exchange(
            b"123-45-678",
            server.salt(),
            &[0x37; 32],
            &server.public_key(),
        );
        let verified = server
            .verify(&controller_public, &controller_proof)
            .unwrap();

        assert_eq!(*verified.session_key, expected_key);
        assert_eq!(
            verified.proof,
            proof_m2(
                &BigUint::from_bytes_be(&controller_public),
                &controller_proof,
                &expected_key,
            )
        );
    }

    #[test]
    fn wrong_setup_code_rejects_proof() {
        let server = SrpServer::new(b"123-45-678", [0x11; 16], [0x5a; 32]).unwrap();
        let (controller_public, controller_proof, _) = test_controller_exchange(
            b"999-99-999",
            server.salt(),
            &[0x37; 32],
            &server.public_key(),
        );

        assert!(matches!(
            server.verify(&controller_public, &controller_proof),
            Err(SrpError::ProofMismatch)
        ));
    }

    #[test]
    fn rejects_zero_public_and_private_values() {
        assert!(matches!(
            SrpServer::new(b"123-45-678", [0x11; 16], [0; 32]),
            Err(SrpError::InvalidPrivateKey)
        ));
        let server = SrpServer::new(b"123-45-678", [0x11; 16], [0x5a; 32]).unwrap();
        assert!(matches!(
            server.verify(&[0], &[0; 64]),
            Err(SrpError::InvalidControllerPublicKey)
        ));
    }
}
