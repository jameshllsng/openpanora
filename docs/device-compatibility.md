# Device compatibility evidence

This file records compatibility by evidence level, not by manufacturer claim.
It intentionally excludes private addresses, serial numbers, credentials, and
certificate exceptions.

| Device / firmware | Device information | Media profiles | Recording Search | Replay | Export |
|---|---|---|---|---|---|
| IntelBras NVD 3332, 4.001.00IB001.0.T (build 2025-08-08) | verified | verified | summary and inventory verified; result polling timed out | URI retrieval verified; RTSP DESCRIBE returned 503; playback unverified | unverified |

The NVD entry was authenticated through HTTPS with HTTP Digest in an authorized
environment. Its certificate was not trusted by that development host, so a
manual diagnostic may need the explicitly requested `--insecure-tls` option.
That fact is not a recommendation to disable certificate validation in normal
deployments.

Next evidence to collect, manually and with sanitized output:

1. Full search-token lifecycle including an empty result set.
2. The final search state and response behavior with the bounded 30-second
   search budget and 45-second requested KeepAliveTime.
3. RTSP `DESCRIBE` behavior for a returned replay URI; the observed 503 is a
   server response, not a playback/decode result.
4. GStreamer replay decode behavior and codecs after RTSP accepts DESCRIBE.
5. Replay clock/range behavior needed to assess export accuracy.
