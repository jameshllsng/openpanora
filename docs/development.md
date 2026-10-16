# Development guide

## Quality gate

```bash
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Keep Rust source focused and split modules by responsibility. Core domain code
must remain independent from GTK and GStreamer.

## Fixtures

Only synthetic or sanitized protocol fixtures may be committed. Remove device
serials, hardware IDs, hostnames, private IPs, usernames, nonces, digests,
cookies, tokens, and recordings. A fixture must identify the behavior it tests,
not the real deployment it came from.

## Device testing

Test only devices explicitly authorized by their owner. `device-probe` does not
perform discovery broadcasts. Future discovery features must require an explicit
network interface or bounded target scope.

Use a hidden password prompt where possible. For automation, pipe a password to
`--password-stdin` without storing it in shell history. Secret Service lookup is
available with `--keyring`; its account key is `<host>:<username>` under service
`org.openpanora.Device`.

## WSL2

Native packages are available on Ubuntu 24.04 under WSL2. GUI execution requires
WSLg or a configured Wayland/X server. GStreamer RTSP testing additionally needs
the relevant demuxer and codec plugin packages. Build success alone is not a GUI
or codec runtime test.
