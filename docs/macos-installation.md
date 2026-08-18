# macOS installation

KeepPeek releases for macOS support Apple Silicon (`arm64`) only.

Download the `keeppeek-<version>-macos-aarch64.dmg` release asset and its
matching `.sha256` file. Verify its checksum:

```sh
shasum -a 256 -c keeppeek-<version>-macos-aarch64.dmg.sha256
```

Open the disk image and drag `KeepPeek.app` to `Applications`. Open KeepPeek
from `Applications` once to install and start its service.

## Service operation

KeepPeek installs a per-user `launchd` service. It starts automatically after
you log in and restarts after an unsuccessful exit. It uses the configuration
and recordings under `~/Library/Application Support/keeppeek`.

To check the service and follow its logs, run:

```sh
launchctl print gui/"$(id -u)"/com.keeppeek
tail -f ~/Library/Logs/KeepPeek/keeppeek.log ~/Library/Logs/KeepPeek/keeppeek-error.log
```

To stop and remove the service and application:

```sh
launchctl bootout gui/"$(id -u)"/com.keeppeek
rm -f ~/Library/LaunchAgents/com.keeppeek.plist
rm -rf /Applications/KeepPeek.app
```

Dragging a newer `KeepPeek.app` to `Applications` replaces the existing
application. Open it once afterward to restart the service using the updated
version.

## Release signing

macOS DMGs are unsigned unless the release workflow is configured with all
three of these GitHub Actions secrets:

- `MACOS_SIGNING_CERTIFICATE_BASE64`: Base64-encoded Developer ID Application
  PKCS#12 certificate.
- `MACOS_SIGNING_CERTIFICATE_PASSWORD`: Password for that certificate.
- `MACOS_SIGNING_IDENTITY`: The certificate's codesigning identity.

When present, the workflow signs the application before creating the DMG.

To submit the signed DMG for notarization and staple the resulting ticket, also
configure these secrets:

- `MACOS_NOTARIZATION_APPLE_ID`: Apple ID used for notarization.
- `MACOS_NOTARIZATION_PASSWORD`: App-specific password for that Apple ID.
- `MACOS_NOTARIZATION_TEAM_ID`: Apple Developer team identifier.
