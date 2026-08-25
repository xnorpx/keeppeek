# External sample video data

The object-detection integration test uses
`sample-videos/person-bicycle-car-detection.mp4` from Intel's archived
[`intel-iot-devkit/sample-videos`](https://github.com/intel-iot-devkit/sample-videos)
repository.

- Upstream commit: `57978890822836f2b4743852f04f62fc511757e4`
- Upstream license: CC-BY-4.0
- Selected video SHA-256: `452b11b7e0efbd019f1d9570d0c790e90416ad4ad29eec6003872d08443140ef`
- Upstream `LICENSE` SHA-256: `e7edaaaeb0cd6da6a3dd97c54f3e8443ef027be685253fab64bf6eed2419ee7e`
- Upstream `README.md` SHA-256: `77eac5b69b1901e7d349005dcdb118d83b1fa592b346f5f985fbae61a7a1123b`

Clone the repository intact from the KeepPeek repository root:

```sh
git clone --depth 1 https://github.com/intel-iot-devkit/sample-videos.git data/sample-videos
test "$(git -C data/sample-videos rev-parse HEAD)" = "57978890822836f2b4743852f04f62fc511757e4"
```

Keep the upstream `data/sample-videos/LICENSE` and `data/sample-videos/README.md` beside the
videos. The external clone is ignored so its 234 MiB of third-party media and nested Git metadata
are not committed to KeepPeek. CI recreates the clone for the real-model integration job and
verifies the pinned commit and hashes before use.
