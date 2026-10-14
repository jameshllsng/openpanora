# Roadmap to 1.0

Statuses: **done**, **in progress**, **not started**, or **blocked on hardware**.

| Requirement | Status | Acceptance direction |
|---|---|---|
| Persistent device registration | in progress | SQLite repository exists; registration UI and channel sync remain |
| Live viewing | in progress | probe decodes RTSP; embedded GTK video session remains |
| 1/4/9/16/36 grids | not started | adaptive grid with substream policy and resource budget |
| Device and channel management | in progress | domain types exist; ONVIF sync and CRUD UI remain |
| Remote recording search | blocked on hardware | HTTPS/Digest service discovery works; implement FindRecordings/GetRecordingSearchResults after sanitized authorized-device fixtures |
| Remote playback | blocked on hardware | Replay XAddr is detected independently but not treated as usable; implement GetReplayUri and GStreamer session after validated responses |
| Calendar/timeline navigation | not started | depends on normalized recording results |
| Time-range export | in progress | core contracts exist; remux/transcode job engine remains |
| MP4 and compatible exports | not started | MP4 remux first; Matroska fallback; test timestamp discontinuities |
| Automatic reconnection | not started | bounded retry with jitter, session state, and cancellation |
| H.264/H.265 | blocked on hardware | GStreamer runtime can decode installed plugins; validate per device |
| Secure credentials | in progress | probe supports hidden prompt/Secret Service retrieval; desktop store UI remains |
| Native Linux interface | in progress | GTK4 shell is launchable; functional workspaces remain |

## Compatibility test procedure

For every authorized model and firmware:

1. Run `device-probe inspect` without credentials and record only classified
   results.
2. If authentication is required, rerun with a hidden prompt or Secret Service.
3. Confirm service namespaces and Media/Media2 profiles.
4. Record Recording Search, Recording Control, and Replay independently.
5. Obtain a stream URI through documented device interfaces, then run
   `rtsp-test`; require a decoded buffer.
6. Test main and substreams, TCP fallback, H.264/H.265, audio, clock drift, and
   reconnect behavior.
7. For recording-capable devices, capture sanitized SOAP fixtures for
   FindRecordings, result paging, GetReplayUri, seek/range, and export behavior.

Missing evidence today: no sanitized Recording Search metadata/available
interval responses, replay URIs, range semantics, or export streams have been
supplied. The next implementation step is to collect those sanitized fixtures
with `device-probe`, then implement and contract-test the three ONVIF recording
services without claiming Profile G universally.
