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

## Embedded live video

Live View requires GStreamer's `gtk4paintablesink`. When `gst-plugin-gtk4` is
built under a user prefix, make it visible before starting the app:

```bash
export GST_PLUGIN_PATH="$HOME/.local/lib/gstreamer-1.0${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}"
gst-inspect-1.0 gtk4paintablesink
cargo run -p openpanora
```

The GTK application owns media pipelines on its main thread; ONVIF and Secret
Service work happens on worker threads. A synthetic `videotestsrc` path covers
the local pipeline assembly test. It does not validate WSLg rendering, RTSP
authentication, H.264/H.265 decoding, or any physical camera. Start real
streams only through an explicit operator action against an authorized device.

Live View only reports **Receiving decoded video** after a pad probe observes
decoded raw buffers before the GTK sink. A running RTSP pipeline is not treated
as proof that frames are visible. If no decoded frame arrives within ten
seconds, it reports a startup stall. GStreamer errors are deliberately reduced
to a safe category because debug text can contain stream addresses.

## Live reconnection

For an explicitly started RTSP live session, transient GStreamer/network errors,
EOS, and a startup with no decoded buffers can trigger at most three automatic
reconnects. Delays are 1, 2, and 4 seconds. Authentication failures, missing
decoders/plugins, unsupported negotiation, and invalid stream setup are
classified as permanent and are not retried automatically. Stop, a profile or
device selection change, and a new Start invalidate all delayed retries. The
user can still use the explicit Retry control after correcting a configuration.

### WSLg diagnostics

WSLg may emit EGL/Zink warnings even when GTK starts. They are environment
diagnostics, not evidence of an RTSP or decoder failure. To independently test
the supported GTK software renderer path, use:

```bash
GSK_RENDERER=cairo GST_PLUGIN_PATH="$HOME/.local/lib/gstreamer-1.0${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}" cargo run -p openpanora
```

This changes GTK rendering only; it does not change RTSP negotiation or make a
missing H.264/H.265 decoder available. The profile chooser is an in-window,
scrollable list rather than a compositor popup, which avoids WSLg popup
placement limitations with large profile inventories.
