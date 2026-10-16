# Architecture

## Goals

OpenPanora is a Linux-first, manufacturer-agnostic VMS client. Live video is
important, but remote recording search, replay, timeline navigation, and export
are architectural requirements rather than optional afterthoughts.

The application follows a clean-room approach. Public standards, documented
vendor interfaces, user-authorized device observations, and independently
written tests are valid inputs. Proprietary binaries, decompiled code, resources,
and copied UI designs are not.

## Workspace boundaries

### `openpanora-core`

Owns domain models and ports. It has no GTK or GStreamer dependency. Current
ports cover device repositories, credential stores, device inspection, live
stream sessions, recording search, playback URI acquisition, and export.

SQLite persistence lives here because it implements a domain port and is usable
by both the desktop application and future background services. Credentials are
represented only by opaque keys in SQLite; secret values belong in Linux Secret
Service or another `CredentialStore` implementation.

### `openpanora`

The GTK4 desktop front end. It owns presentation and user-driven orchestration.
The initial shell exposes only working navigation and the SQLite device list.
Live and recording controls remain informational until connected implementations
exist.

### `device-probe`

A deliberately narrow diagnostic. It targets exactly one operator-supplied host
or RTSP URL and never performs implicit network-wide discovery. It uses ONVIF
SOAP calls to inventory services and media profiles and GStreamer to prove that
an RTSP stream reaches decoded media buffers.

Its ONVIF transport keeps HTTP and SOAP authentication independent. HTTPS is
verified by default; HTTP Digest challenge-response is negotiated per request,
and WS-Security UsernameToken PasswordDigest remains available as SOAP-layer
authentication. A service XAddr is used only after validation, and an HTTPS
Device Management endpoint is never downgraded to an HTTP service endpoint.

## Target component model

```text
GTK4 application
    |
Application/use-case layer
    +-- Device repository (SQLite)
    +-- Credential store (Secret Service)
    +-- ONVIF standards adapter
    |     +-- Device / Media / Media2
    |     +-- Recording Search / Recording Control / Replay
    |     +-- PTZ / Events
    +-- Media sessions (GStreamer)
    +-- Export jobs (remux first, transcode only when needed)
    +-- Optional vendor adapters (out-of-process when practical)
```

## Decisions

### Standards-first, capability-driven

No brand implies a capability. Each model/firmware pair is probed and cached.
The presence of ONVIF does not imply Profile G. Recording Search, Recording
Control, and Replay are reported independently.

### RTSP success means decoded media

Opening a socket is insufficient. The probe uses a GStreamer pipeline and
requires at least one decoded buffer. Future tests will retain codec, negotiated
transport, latency, and reconnect observations without logging credentials.

### Recording and export model

Remote search returns normalized time-bounded segments. Playback first obtains a
device-provided replay URI, then hands media transport to GStreamer. Export is a
job over an explicit UTC interval. MP4 remux without transcoding is preferred;
Matroska is a fallback where codec/container constraints require it.

The probe owns a separate read-only recording investigation adapter. It resolves
the service XAddr from `GetServices`, accepts only HTTP(S) SOAP service URLs on
the selected device host by default, and refuses HTTPS-to-HTTP downgrade. An
operator may approve one alternate service host explicitly for unusual ONVIF
deployments. Search sessions are bounded by a total deadline and page limit,
and an `EndSearch` cleanup request is attempted after each successfully-created
search token. The desktop application will consume the same typed results
rather than interpreting SOAP XML in its UI layer.

### Vendor extensions

Vendor adapters implement the same core ports. They are optional and should be
isolated from the UI and standards adapter. An adapter is accepted only with a
documented, licensed interface and tests against explicitly authorized hardware.

### Secrets and diagnostics

SQLite stores credential references, never passwords. CLI secrets are accepted
through a hidden prompt, standard input, or Secret Service. Diagnostic output
redacts device serial and hardware identifiers and must never print SOAP security
headers or credential-bearing URLs.

## Known gaps

- WS-Discovery is not yet implemented. When added, it must require an explicit
  interface or target scope and must not silently scan every network.
- ONVIF replay/search request bodies require validation against real authorized
  devices before implementation.
- The GTK shell does not yet host a GStreamer video widget.
- Reconnection policy, hardware decoding selection, and multi-camera resource
  budgeting are not implemented.
