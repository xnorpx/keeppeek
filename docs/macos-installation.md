# macOS installation

KeepPeek releases for macOS support Apple Silicon (`arm64`) only.

Download the `keeppeek-<version>-macos-aarch64.tar.gz` release asset and its
matching `.sha256` file. Verify the checksum, extract the archive, and change
to the extracted directory:

```sh
shasum -a 256 -c keeppeek-<version>-macos-aarch64.tar.gz.sha256
tar xzf keeppeek-<version>-macos-aarch64.tar.gz
cd keeppeek-<version>-macos-aarch64
```

## Manual operation

Install the executable for the current user:

```sh
./install-macos.sh
```

It is installed at `~/.local/bin/keeppeek`. Add that directory to your `PATH`
if necessary, then start KeepPeek in a terminal:

```sh
keeppeek
```

KeepPeek stops cleanly when the terminal receives Ctrl+C. Its configuration
and default recordings are stored in
`~/Library/Application Support/keeppeek`.

To use a different installation directory, pass `--prefix`:

```sh
./install-macos.sh --prefix /path/to/bin
```

## Service operation

The installer can instead install a system `launchd` service that runs
KeepPeek as a local user. It starts at boot and is restarted if KeepPeek exits
unsuccessfully. Run it from an administrator account:

```sh
sudo ./install-macos.sh --service --user "$(id -un)"
```

The service executable is `/usr/local/bin/keeppeek`, its launchd label is
`com.keeppeek`, and it uses the selected user's KeepPeek configuration. Check
its status and logs with:

```sh
sudo launchctl print system/com.keeppeek
sudo tail -f /var/log/keeppeek.log /var/log/keeppeek-error.log
```

To stop and remove the service:

```sh
sudo launchctl bootout system/com.keeppeek
sudo rm -f /Library/LaunchDaemons/com.keeppeek.plist /usr/local/bin/keeppeek
```

Run the service installation command again to upgrade an existing service.
