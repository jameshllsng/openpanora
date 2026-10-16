# Remote recordings and export feasibility

## Capability vocabulary

- **Advertised**: `GetServices` returned an XAddr for the relevant ONVIF
  namespace. It is not evidence that an operation works.
- **Operation verified**: an authorized probe request returned a valid response
  for that operation on a specific model and firmware.
- **Playback verified**: GStreamer received a decoded media buffer from a
  device-provided replay URI.
- **Export verified**: an exported file has been checked for requested bounds,
  duration, decodability, and container integrity.

## Implemented read-only investigation flow

`device-probe recordings summary` sends `GetRecordingSummary` to the ONVIF
Recording Search service. `list` sends Recording Control `GetRecordings`.
`search` sends `FindRecordings`, uses the returned search token with bounded
`GetRecordingSearchResults` polling, looks up missing interval metadata with
`GetRecordingInformation`, then attempts `EndSearch`.

The interval arguments are RFC 3339 UTC values. ONVIF `FindRecordings` has no
time-range argument, so OpenPanora filters its discovered recording metadata by
interval. A missing `EarliestRecording` or `LatestRecording` is never treated
as a positive time-range match.

Searches use `MinResults=1`, bounded `WaitTime`, bounded inter-poll delay, and
a separately bounded session `KeepAliveTime`. `GetRecordingSearchResults` is
allowed to report `Searching`; it is not a failure by itself. If the poll limit
or total search budget is reached, the probe reports the final observed state
and still makes one bounded `EndSearch` cleanup attempt.

`replay-uri` sends `GetReplayUri` requesting unicast RTP over RTSP. A returned
URI is accepted only when it is RTSP/RTSPS, credential-free, and hosted by the
selected device or an explicitly approved alternate service host. `replay-test`
uses that URI unchanged (including its opaque query parameters), first performs
read-only RTSP `OPTIONS`/`DESCRIBE` authentication checks, then configures
GStreamer `rtspsrc` for RTP/TCP and ONVIF mode before requiring a decoded
buffer. Diagnostics identify whether the failure is before RTSP authentication,
before `DESCRIBE`, or during later GStreamer startup/decoding. Neither command
changes recording configuration or downloads video.

ONVIF replay start uses an RTSP `Range: clock=...` request at `PLAY`; it is not
equivalent to an ordinary live RTSP seek. The `replay-test --start <UTC>` option
therefore currently stops with an explicit unsupported result rather than
silently ignoring the timestamp or emitting an unverified range. This remains
the next device-validation step.

## Current export conclusion

No export proof of concept is enabled yet. An ONVIF Replay URI can identify a
recorded stream, but it does not prove that a requested UTC start/end maps to
exact media boundaries. The following evidence is required before implementing
an MP4 remux job:

1. A successful `GetReplayUri` response and decoded replay stream.
2. Device-specific replay range semantics: RTSP clock range, seek behavior,
   actual first/last decoded timestamps, and keyframe alignment.
3. Sanitized fixtures showing recording/track metadata and discontinuities.
4. GStreamer validation of `rtspsrc ! depay ! parse ! mp4mux` for the returned
   codec(s), including H.264/H.265 and optional audio.
5. Output validation: temporary destination, cancellation, progress from media
   timestamps, atomic rename only after EOS, cleanup of partial files, and a
   final decode/container check.

An initial export implementation should be an explicit job with UTC bounds,
user-selected destination, cancellation token, and no automatic download. It
should remux only if timestamps and codecs are compatible; otherwise it must
offer a clearly-labelled transcode or unsupported result. It must not claim
frame-accurate boundaries before hardware validation.

## Safety boundaries

No raw SOAP bodies are saved by default. Search tokens are never shown;
human-readable recording tokens are redacted and the full values appear only in
an explicitly requested JSON response. Neither is persisted. Passwords, HTTP
authorization headers, WS-Security headers, and replay URI query strings are
never printed. TLS validation remains enabled unless the operator explicitly
uses the local-diagnostic `--insecure-tls` flag. A PEM CA file can instead be
supplied through `--tls-ca-file` to preserve certificate verification.
