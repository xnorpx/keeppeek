# Third-Party Sources

The internal TLV8 codec is a KeepPeek implementation based on behavior and tests from `hap-tlv8 1.0.0` in <https://github.com/phunapps/hap-rust>, commit `2195a348d24ddfa7aeb295264287a2f402e0c52c`, licensed under Apache-2.0.

The encrypted HAP record framing and control-key labels are based on `hap-crypto 1.4.0` and `hap-transport 1.3.0` from the same upstream project and on `hap 0.1.0-pre.15` from <https://github.com/ewilken/hap-rs>, commit `bfc3aa30520663d9cf97ff7a5ab2ce608e614764`, licensed under MIT OR Apache-2.0. Cryptographic primitives are provided by RustCrypto crates.

The accessory-side Pair Verify state machine is a KeepPeek implementation informed by the controller-side `hap-crypto 1.4.0` state machine and the accessory-side Pair Verify handler in `hap 0.1.0-pre.15`. Ed25519, X25519, HKDF-SHA512, and ChaCha20-Poly1305 primitives are provided by RustCrypto crates.

The HAP-specific SRP-6a server is informed by `hap-crypto 1.4.0` and uses the RFC 5054 3072-bit group bytes from RustCrypto `srp 0.6.0` (SHA-256 `48cf8b092fbce4359d9871abf74f98e25b6163379eaa15cd9087e800c6d1c55c`), licensed under MIT OR Apache-2.0. Private big integers use `num-bigint-dig` with zeroization enabled.

The accessory database JSON shape and permission/format spellings are informed by `hap-model 1.2.0` and `hap 0.1.0-pre.15`. The 2026 Camera WebRTC Stream Management UUIDs and tier values come from Apple's public HomeKit Secure Video Open Source Compatibility Guide dated June 3, 2026.

The HAP HTTP endpoint/content-type mapping is informed by `hap-transport 1.3.0` and `hap 0.1.0-pre.15`. HTTP/1.1 syntax parsing is delegated to the independently maintained `httparse` crate.

WebRTC offer/answer negotiation, ICE, DTLS-SRTP, RTP packetization, bandwidth estimation, and media transport state are provided by `str0m 0.23.0`, licensed under MIT OR Apache-2.0. KeepPeek owns the HomeKit-specific orchestration and the socket/thread adapter around str0m's sans-I/O API.

The temporary local `hap` and `hap-controller` reference trees were removed after the relevant protocol behavior was consolidated. Runtime code in this crate does not depend on either package.
