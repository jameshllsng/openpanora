# Device compatibility evidence

This file records compatibility by evidence level, not by manufacturer claim.
It intentionally excludes private addresses, serial numbers, credentials, and
certificate exceptions.

| Device / firmware | Device information | Media profiles | Recording Search | Replay | Export |
|---|---|---|---|---|---|
| IntelBras NVD 3332, 4.001.00IB001.0.T (build 2025-08-08) | verified | verified | summary and inventory verified; search execution unverified | URI retrieval verified; playback unverified | unverified |

The NVD entry was authenticated through HTTPS with HTTP Digest in an authorized
environment. Its certificate was not trusted by that development host, so a
manual diagnostic may need the explicitly requested `--insecure-tls` option.
That fact is not a recommendation to disable certificate validation in normal
deployments.

Next evidence to collect, manually and with sanitized output:

1. Full search-token lifecycle including an empty result set.
2. RTSP `OPTIONS`/`DESCRIBE` authentication for a returned replay URI.
3. GStreamer replay decode behavior and codecs.
4. Replay clock/range behavior needed to assess export accuracy.
