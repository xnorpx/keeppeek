# ONVIF-rs

ONVIF-rs is a Rust client library implementation of the ONVIF specification.

## Features

- [x] all ONVIF types are generated from official schema
- [ ] all ONVIF operations are generated from official schema
- [x] operations are synchronous and do not require an async runtime
- [x] device discovery on the local network using _WS-discovery_ which is mandatory for all ONVIF devices
- [x] authentication using _WS-Security UsernameToken_ which is mandatory for all ONVIF devices
- [x] zero unsafe

## Usage

Cargo.toml:

```toml
[dependencies]
onvif = { git = "https://github.com/lumeohq/onvif-rs" }
```

## Runtime and TLS

The SOAP client uses blocking `ureq` requests. Generated service operations and
`DiscoveryBuilder::discover()` are synchronous, so callers do not need Tokio or
another async runtime.

HTTPS certificate verification uses the platform verifier by default. Cameras
with self-signed certificates require an explicit opt-in:

```rust
use onvif::soap::client::{ClientBuilder, TlsVerification};

let client = ClientBuilder::new(&uri)
    .tls_verification(TlsVerification::AcceptInvalidCertificates)
    .build();
```

## Examples

To [discover](onvif/examples/discovery.rs) devices on the local network:

```shell script
cargo run --example discovery
```

To [inspect and control a camera](onvif/examples/camera.rs):

```shell script
cargo run --example camera -- help

cargo run --example camera -- get-system-date-and-time \
    --uri=http://192.168.0.2:8000

cargo run --example camera -- set-hostname \
    --uri=http://192.168.0.2:8000 --username=admin --password=qwerty cam2

cargo run --example camera -- get-stream-uris --uri=http://192.168.0.2:8000
```

To [pull events](onvif/examples/event.rs) from a camera, adjust credentials in event.rs and run:

```shell script
cargo run --example event
```

## Dependencies

- XSD -> Rust code generation: [xsd-parser-rs](https://github.com/lumeohq/xsd-parser-rs)
- XML (de)serialization: [yaserde](https://github.com/media-io/yaserde)

## Contributing

Pull requests are welcome. For major changes, please open an issue first to discuss what you would like to change.

Please make sure to update tests as appropriate.

## License

[MIT](LICENSE)
