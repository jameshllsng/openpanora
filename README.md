# OpenPanora

OpenPanora is an open-source, Linux-first desktop application for managing and
monitoring IP cameras, DVRs, and NVRs. It prioritizes documented ONVIF and
RTSP/RTP interfaces and keeps vendor integrations optional and isolated.

The project is in its initial development sprint. Device compatibility has not
yet been verified against physical hardware.

## Current functionality

- A launchable GTK4 desktop shell with Devices, Live View, Recordings, and
  Diagnostics workspaces.
- A SQLite-backed device repository and domain contracts for live streaming,
  recording search, replay, and time-range export.
- `device-probe`, which targets one explicitly supplied host and performs real
  ONVIF Device Management and Media requests.
- HTTP Digest (`qop=auth`) negotiation for ONVIF services, while retaining
  WS-Security PasswordDigest for devices that require SOAP authentication.
- ONVIF service classification for Recording Search, Recording Control, Replay,
  Media, PTZ, and Events.
- Read-only Recording Search summary, recording configuration inventory,
  asynchronous recording search, and Replay URI investigation through
  `device-probe`.
- RTSP validation through GStreamer. Success requires a decoded media buffer,
  not merely a successful TCP connection.

## Version 1.0 goals

- Device and channel management
- Live viewing with 1, 4, 9, 16 and 36-camera layouts
- Remote recording search and playback
- Timeline and calendar navigation
- Video export by selected time range
- H.264/H.265 support and automatic reconnection
- Secure credential storage

See [the roadmap](docs/roadmap.md) for acceptance criteria and current status.

## Prerequisites

Ubuntu 24.04:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libgtk-4-dev \
  libgstreamer1.0-dev libsecret-1-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad gstreamer1.0-libav
```

Install the stable Rust toolchain using rustup. The workspace uses Rust 2024
edition and requires Rust 1.85 or newer.

## Build and test

```bash
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Run the desktop application

```bash
cargo run -p openpanora
```

WSL2 requires WSLg or another functioning Wayland/X11 display. Compilation and
unit tests do not prove that the GUI can open on a particular WSL installation.

## Device probe

The probe never scans the network. Every request targets the host or RTSP URL
supplied by the operator.

```bash
cargo run -p device-probe -- inspect --host 192.168.10.50
cargo run -p device-probe -- capabilities --host 192.168.10.50 --username admin
cargo run -p device-probe -- inspect --service-url 'https://camera.example/onvif/device_service' --username admin
cargo run -p device-probe -- recordings summary --service-url 'https://camera.example/onvif/device_service' --username admin
cargo run -p device-probe -- recordings search --service-url 'https://camera.example/onvif/device_service' --username admin --start '2026-01-01T00:00:00Z' --end '2026-01-02T00:00:00Z'
cargo run -p device-probe -- recordings replay-uri --service-url 'https://camera.example/onvif/device_service' --username admin --recording-token '<TOKEN>'
cargo run -p device-probe -- recordings replay-test --service-url 'https://camera.example/onvif/device_service' --username admin --recording-token '<TOKEN>'
cargo run -p device-probe -- --timeout 20 rtsp-test --url 'rtsp://camera.example/stream'
```

When `--username` is used, the password is prompted without echo. For
automation, use `--password-stdin`, or retrieve an existing Secret Service entry
with `--keyring`. Never put passwords in command-line URLs or repository files.

HTTPS certificate verification is enabled by default. `--insecure-tls` is a
deliberately explicit local-diagnostic escape hatch for a known, authorized
device with a self-signed certificate; it must never be used as a deployment
default. It is accepted only for HTTPS ONVIF endpoints. Prefer
`--tls-ca-file /path/to/device-ca.pem` when a device certificate or local CA can
be trusted explicitly.

An advertised Recording Search, Recording Control, or Replay service is
reported as **unverified** until its relevant operation has actually succeeded.
The probe executes only read-only investigation operations when the operator
explicitly requests a recording command; it does not claim the resulting
capability is usable until that command succeeds on the selected device.

Recording Search is read-only. The probe bounds polling, uses a ten-second
ONVIF keep-alive request, and attempts `EndSearch` after a session was created.
`--start` and `--end` filter recording metadata returned by the device; they do
not claim frame-accurate video boundaries. See
[remote-recordings.md](docs/remote-recordings.md) for compatibility and export
evidence required before enabling a user-facing export feature. Replay tests first perform redacted RTSP `OPTIONS` and
`DESCRIBE` checks (including Digest when challenged), then require GStreamer to
decode a buffer over RTP/TCP. A replay test timeout reports the last completed
stage. `replay-test --start <UTC>` is accepted as an explicit investigation
parameter but currently refuses to send a clock `Range` request until that
device's replay semantics are verified.

Exit codes are `0` for success, `2` for invalid input, `4` for authentication,
`5` for timeouts, `6` for unsupported operations, and `1` for other failures.

## Safety and privacy

- Test only devices you own or are explicitly authorized to access.
- Do not commit credentials, private address inventories, packet captures, or
  recordings.
- Serial numbers and hardware identifiers are detected but redacted by the
  compatibility report.
- Vendor-specific adapters must use licensed, documented interfaces. OpenPanora
  does not contain Intelbras code, DLLs, UI assets, or decompiled source.

## Project structure

```text
crates/core          Domain contracts and SQLite persistence
crates/app           GTK4 desktop application
tools/device-probe   ONVIF and RTSP compatibility diagnostic
docs                 Architecture, development, and roadmap
```

See [architecture.md](docs/architecture.md) and
[development.md](docs/development.md) before making architectural changes.

## Contributing

Run all four quality commands before committing. Keep fixture data synthetic and
sanitized. Report device behavior by exact model and firmware when possible.

## License status

No final project license has been selected. GPLv3 is under consideration, but
license selection requires an explicit maintainer decision.
