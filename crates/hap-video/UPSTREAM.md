# Upstream References

This crate consolidates the relevant accessory protocol behavior from:

- `hap 0.1.0-pre.15`, <https://github.com/ewilken/hap-rs>, commit `bfc3aa30520663d9cf97ff7a5ab2ce608e614764`, MIT OR Apache-2.0.
- `hap-controller 3.1.0` and its lower protocol crates, <https://github.com/phunapps/hap-rust>, commit `2195a348d24ddfa7aeb295264287a2f402e0c52c`, Apache-2.0.
- RustCrypto `srp 0.6.0`, <https://github.com/RustCrypto/PAKEs>, for the RFC 5054 3072-bit group bytes, MIT OR Apache-2.0.

KeepPeek owns the resulting sans-I/O APIs and state machines. No runtime dependency on either HAP project remains. See [THIRD_PARTY.md](THIRD_PARTY.md) for component-level attribution.
