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
- ONVIF service classification for Recording Search, Recording Control, Replay,
  Media, PTZ, and Events.
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
cargo run -p device-probe -- --timeout 20 rtsp-test --url 'rtsp://camera.example/stream'
```

When `--username` is used, the password is prompted without echo. For
automation, use `--password-stdin`, or retrieve an existing Secret Service entry
with `--keyring`. Never put passwords in command-line URLs or repository files.

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
