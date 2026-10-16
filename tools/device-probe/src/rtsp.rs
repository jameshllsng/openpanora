use crate::{auth::Credentials, onvif::ProbeError};
use gst::prelude::*;
use gstreamer as gst;
use md5::{Digest, Md5};
use rand::RngCore;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;

const MAX_RTSP_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayStage {
    UriPrepared,
    RtspTcpConnected,
    RtspOptionsCompleted,
    RtspDescribeCompleted,
    GstreamerSourceCreated,
    GstreamerPreroll,
    GstreamerAsyncDone,
    DecodedBuffer,
}

impl ReplayStage {
    fn label(self) -> &'static str {
        match self {
            Self::UriPrepared => "replay URI prepared",
            Self::RtspTcpConnected => "RTSP TCP connection",
            Self::RtspOptionsCompleted => "RTSP OPTIONS",
            Self::RtspDescribeCompleted => "RTSP DESCRIBE",
            Self::GstreamerSourceCreated => "GStreamer RTSP source setup",
            Self::GstreamerPreroll => "GStreamer preroll",
            Self::GstreamerAsyncDone => "GStreamer asynchronous startup",
            Self::DecodedBuffer => "decoded media buffer",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayDiagnostic {
    pub stage: ReplayStage,
    pub elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
pub struct RtspTestResult {
    pub decoded_buffer_observed: bool,
    pub elapsed_ms: u128,
    pub rtsp_transport: &'static str,
    pub onvif_mode_enabled: bool,
    pub replay_query_preserved: bool,
    pub diagnostics: Vec<ReplayDiagnostic>,
}

#[derive(Debug, Clone, Copy)]
pub struct ReplayTestOptions {
    pub timeout: Duration,
    pub start_requested: bool,
}

impl ReplayTestOptions {
    pub fn new(timeout: Duration, start_requested: bool) -> Self {
        Self {
            timeout,
            start_requested,
        }
    }
}

pub fn test(url: &Url, timeout: Duration) -> Result<RtspTestResult, ProbeError> {
    test_internal(url, None, ReplayTestOptions::new(timeout, false))
}

pub fn test_replay_with_credentials(
    url: &Url,
    credentials: Option<&Credentials>,
    options: ReplayTestOptions,
) -> Result<RtspTestResult, ProbeError> {
    if options.start_requested {
        return Err(ProbeError::Unsupported(
            "absolute replay start requires verified RTSP clock-range negotiation; no Range header was sent"
                .into(),
        ));
    }
    test_internal(url, credentials, options)
}

fn test_internal(
    url: &Url,
    credentials: Option<&Credentials>,
    options: ReplayTestOptions,
) -> Result<RtspTestResult, ProbeError> {
    let started = Instant::now();
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    record_stage(&diagnostics, started, ReplayStage::UriPrepared);
    let playback_uri = playback_uri(url, credentials)?;
    let replay_query_preserved = url.query() == playback_uri.query();
    if !replay_query_preserved {
        return Err(ProbeError::Media(
            "replay URI query parameters changed before GStreamer startup".into(),
        ));
    }

    // OPTIONS/DESCRIBE are read-only preflight operations. They establish RTSP account
    // compatibility and separate setup/play/decode stalls from RTSP negotiation failures.
    rtsp_preflight(url, credentials, options.timeout, started, &diagnostics)?;
    run_gstreamer(
        &playback_uri,
        options.timeout,
        started,
        diagnostics,
        replay_query_preserved,
    )
}

fn playback_uri(url: &Url, credentials: Option<&Credentials>) -> Result<Url, ProbeError> {
    let mut playback_uri = url.clone();
    if let Some(credentials) = credentials {
        playback_uri
            .set_username(&credentials.username)
            .map_err(|_| ProbeError::Input("RTSP username cannot be represented safely".into()))?;
        playback_uri
            .set_password(Some(&credentials.password))
            .map_err(|_| ProbeError::Input("RTSP password cannot be represented safely".into()))?;
    }
    Ok(playback_uri)
}

fn rtsp_preflight(
    url: &Url,
    credentials: Option<&Credentials>,
    timeout: Duration,
    started: Instant,
    diagnostics: &Arc<Mutex<Vec<ReplayDiagnostic>>>,
) -> Result<(), ProbeError> {
    if url.scheme() == "rtsps" {
        // A custom preflight would bypass GStreamer's RTSPS certificate verification.
        return Ok(());
    }
    if url.scheme() != "rtsp" {
        return Err(ProbeError::Input(
            "replay URI must use rtsp or rtsps".into(),
        ));
    }
    let mut stream = connect_rtsp(
        url,
        remaining_timeout(timeout, started, ReplayStage::RtspTcpConnected)?,
        started,
    )?;
    record_stage(diagnostics, started, ReplayStage::RtspTcpConnected);
    let target = request_target(url);
    let first = send_rtsp_request(
        &stream,
        "OPTIONS",
        &target,
        1,
        None,
        remaining_timeout(timeout, started, ReplayStage::RtspOptionsCompleted)?,
    )
    .map_err(|error| rtsp_operation_error(error, started, ReplayStage::RtspOptionsCompleted))?;
    let challenge = match first.status {
        200..=299 => None,
        401 => Some(parse_rtsp_digest_challenge(&first)?),
        status => return Err(rtsp_status_error("OPTIONS", status)),
    };
    let mut reconnect_before_describe = response_closes_connection(&first);
    if let Some(challenge) = &challenge {
        let credentials = credentials.ok_or(ProbeError::HttpUnauthorized)?;
        if reconnect_before_describe {
            stream = connect_rtsp(
                url,
                remaining_timeout(timeout, started, ReplayStage::RtspOptionsCompleted)?,
                started,
            )?;
        }
        let authorization =
            rtsp_digest_authorization(credentials, challenge, "OPTIONS", &target, 2);
        let reply = send_rtsp_request(
            &stream,
            "OPTIONS",
            &target,
            2,
            Some(&authorization),
            remaining_timeout(timeout, started, ReplayStage::RtspOptionsCompleted)?,
        )
        .map_err(|error| rtsp_operation_error(error, started, ReplayStage::RtspOptionsCompleted))?;
        if reply.status == 401 {
            return Err(ProbeError::InvalidCredentials);
        }
        if !(200..=299).contains(&reply.status) {
            return Err(rtsp_status_error("OPTIONS", reply.status));
        }
        reconnect_before_describe = response_closes_connection(&reply);
    }
    record_stage(diagnostics, started, ReplayStage::RtspOptionsCompleted);
    if reconnect_before_describe {
        stream = connect_rtsp(
            url,
            remaining_timeout(timeout, started, ReplayStage::RtspDescribeCompleted)?,
            started,
        )?;
    }
    let authorization = challenge.as_ref().map(|challenge| {
        let credentials = credentials.expect("credentials required when digest is challenged");
        rtsp_digest_authorization(credentials, challenge, "DESCRIBE", &target, 3)
    });
    let describe = send_rtsp_request(
        &stream,
        "DESCRIBE",
        &target,
        3,
        authorization.as_deref(),
        remaining_timeout(timeout, started, ReplayStage::RtspDescribeCompleted)?,
    )
    .map_err(|error| rtsp_operation_error(error, started, ReplayStage::RtspDescribeCompleted))?;
    if describe.status == 401 {
        return Err(if credentials.is_some() {
            ProbeError::InvalidCredentials
        } else {
            ProbeError::HttpUnauthorized
        });
    }
    if !(200..=299).contains(&describe.status) {
        return Err(rtsp_status_error("DESCRIBE", describe.status));
    }
    record_stage(diagnostics, started, ReplayStage::RtspDescribeCompleted);
    Ok(())
}

fn run_gstreamer(
    playback_uri: &Url,
    timeout: Duration,
    started: Instant,
    diagnostics: Arc<Mutex<Vec<ReplayDiagnostic>>>,
    replay_query_preserved: bool,
) -> Result<RtspTestResult, ProbeError> {
    gst::init().map_err(|error| ProbeError::Media(error.to_string()))?;
    let observed = Arc::new(AtomicBool::new(false));
    let video_sink = fake_sink(observed.clone(), diagnostics.clone(), started)?;
    let audio_sink = fake_sink(observed.clone(), diagnostics.clone(), started)?;
    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", playback_uri.as_str())
        .property("video-sink", &video_sink)
        .property("audio-sink", &audio_sink)
        .build()
        .map_err(|error| ProbeError::Media(error.to_string()))?;
    configure_replay_source(&playbin, diagnostics.clone(), started);
    let bus = playbin
        .bus()
        .ok_or_else(|| ProbeError::Media("GStreamer playbin has no message bus".into()))?;
    playbin
        .set_state(gst::State::Playing)
        .map_err(|error| ProbeError::Media(error.to_string()))?;

    let outcome = loop {
        if observed.load(Ordering::Acquire) {
            break Ok(RtspTestResult {
                decoded_buffer_observed: true,
                elapsed_ms: started.elapsed().as_millis(),
                rtsp_transport: "tcp",
                onvif_mode_enabled: true,
                replay_query_preserved,
                diagnostics: snapshot_diagnostics(&diagnostics),
            });
        }
        let remaining = match remaining_timeout(timeout, started, current_stage(&diagnostics)) {
            Ok(remaining) => remaining,
            Err(error) => break Err(error),
        };
        if let Some(message) = bus.timed_pop(gst::ClockTime::from_nseconds(
            remaining
                .min(Duration::from_millis(250))
                .as_nanos()
                .min(u64::MAX as u128) as u64,
        )) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    break Err(classify_stream_error(
                        &error.error().to_string(),
                        current_stage(&diagnostics),
                    ));
                }
                gst::MessageView::Eos(_) if !observed.load(Ordering::Acquire) => {
                    break Err(ProbeError::Media(format!(
                        "stream ended during {} before GStreamer decoded a media buffer",
                        current_stage(&diagnostics).label()
                    )));
                }
                gst::MessageView::AsyncDone(_) => {
                    record_stage(&diagnostics, started, ReplayStage::GstreamerAsyncDone);
                }
                gst::MessageView::StateChanged(state)
                    if state
                        .src()
                        .is_some_and(|source| source == playbin.upcast_ref::<gst::Object>())
                        && state.current() == gst::State::Paused =>
                {
                    record_stage(&diagnostics, started, ReplayStage::GstreamerPreroll);
                }
                _ => {}
            }
        }
    };
    let _ = playbin.set_state(gst::State::Null);
    outcome
}

fn configure_replay_source(
    playbin: &gst::Element,
    diagnostics: Arc<Mutex<Vec<ReplayDiagnostic>>>,
    started: Instant,
) {
    playbin.connect("source-setup", false, move |values| {
        if let Ok(source) = values[1].get::<gst::Element>() {
            if source
                .factory()
                .is_some_and(|factory| factory.name() == "rtspsrc")
            {
                // TCP avoids RTP UDP/NAT ambiguity. ONVIF mode makes rtspsrc use ONVIF
                // replay negotiation, including the Rate-Control header.
                source.set_property_from_str("protocols", "tcp");
                source.set_property("onvif-mode", true);
                source.set_property("onvif-rate-control", true);
                record_stage(&diagnostics, started, ReplayStage::GstreamerSourceCreated);
            }
        }
        None
    });
}

fn remaining_timeout(
    timeout: Duration,
    started: Instant,
    stage: ReplayStage,
) -> Result<Duration, ProbeError> {
    let elapsed = started.elapsed();
    let remaining = timeout.saturating_sub(elapsed);
    if remaining.is_zero() {
        Err(ProbeError::OperationTimeout {
            operation: format!("RTSP replay {}", stage.label()),
            elapsed_ms: elapsed.as_millis(),
        })
    } else {
        Ok(remaining)
    }
}

fn rtsp_operation_error(error: ProbeError, started: Instant, stage: ReplayStage) -> ProbeError {
    match error {
        ProbeError::Timeout => ProbeError::OperationTimeout {
            operation: format!("RTSP replay {}", stage.label()),
            elapsed_ms: started.elapsed().as_millis(),
        },
        other => other,
    }
}

fn connect_rtsp(url: &Url, timeout: Duration, started: Instant) -> Result<TcpStream, ProbeError> {
    let host = url
        .host_str()
        .ok_or_else(|| ProbeError::Input("RTSP URI must include a host".into()))?;
    let port = url.port_or_known_default().unwrap_or(554);
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|_| ProbeError::Transport("RTSP host resolution failed".into()))?;
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    if started.elapsed() >= timeout {
        Err(ProbeError::OperationTimeout {
            operation: "RTSP replay TCP connection".into(),
            elapsed_ms: started.elapsed().as_millis(),
        })
    } else {
        Err(ProbeError::Transport(format!(
            "RTSP TCP connection failed: {}",
            last_error.map_or_else(|| "no address resolved".into(), |error| error.to_string())
        )))
    }
}

fn request_target(url: &Url) -> String {
    let mut target = url.path().to_owned();
    if target.is_empty() {
        target.push('/');
    }
    if let Some(query) = url.query() {
        target.push('?');
        target.push_str(query);
    }
    target
}

struct RtspResponse {
    status: u16,
    headers: Vec<(String, String)>,
}

fn response_closes_connection(response: &RtspResponse) -> bool {
    response
        .headers
        .iter()
        .any(|(name, value)| name == "connection" && value.eq_ignore_ascii_case("close"))
}

fn send_rtsp_request(
    stream: &TcpStream,
    method: &str,
    target: &str,
    cseq: u32,
    authorization: Option<&str>,
    timeout: Duration,
) -> Result<RtspResponse, ProbeError> {
    stream
        .set_read_timeout(Some(timeout))
        .map_err(rtsp_io_error)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(rtsp_io_error)?;
    let mut request = format!(
        "{method} {target} RTSP/1.0\r\nCSeq: {cseq}\r\nUser-Agent: OpenPanora-device-probe\r\n"
    );
    if let Some(authorization) = authorization {
        request.push_str("Authorization: ");
        request.push_str(authorization);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    let mut stream = stream;
    stream
        .write_all(request.as_bytes())
        .map_err(rtsp_io_error)?;
    stream.flush().map_err(rtsp_io_error)?;
    read_rtsp_response(&mut stream).map_err(rtsp_io_error)
}

fn read_rtsp_response(stream: &mut &TcpStream) -> std::io::Result<RtspResponse> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "RTSP response ended before headers",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_RTSP_HEADER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "RTSP response headers exceed limit",
            ));
        }
    }
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header separator was checked")
        + 4;
    let header = std::str::from_utf8(&bytes[..header_end]).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "RTSP response headers are not UTF-8",
        )
    })?;
    let mut lines = header.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "RTSP response has no status",
            )
        })?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    Ok(RtspResponse { status, headers })
}

fn rtsp_io_error(error: std::io::Error) -> ProbeError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        ProbeError::Timeout
    } else {
        ProbeError::Transport(format!("RTSP protocol I/O failed: {error}"))
    }
}

fn rtsp_status_error(operation: &str, status: u16) -> ProbeError {
    match status {
        401 => ProbeError::HttpUnauthorized,
        403 => ProbeError::HttpForbidden,
        405 | 461 => ProbeError::Unsupported(format!(
            "RTSP {operation} was rejected with status {status}"
        )),
        _ => ProbeError::RtspServerResponse {
            operation: operation.to_owned(),
            status,
        },
    }
}

#[derive(Debug)]
struct RtspDigestChallenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
}

fn parse_rtsp_digest_challenge(response: &RtspResponse) -> Result<RtspDigestChallenge, ProbeError> {
    let challenge = response
        .headers
        .iter()
        .find(|(name, value)| {
            name == "www-authenticate" && value.to_ascii_lowercase().starts_with("digest ")
        })
        .map(|(_, value)| value)
        .ok_or_else(|| {
            ProbeError::Unsupported("RTSP authentication does not offer Digest".into())
        })?;
    let parameters = parse_digest_parameters(&challenge[7..]);
    let realm = parameters
        .get("realm")
        .cloned()
        .ok_or_else(|| ProbeError::Malformed("RTSP Digest challenge omitted realm".into()))?;
    let nonce = parameters
        .get("nonce")
        .cloned()
        .ok_or_else(|| ProbeError::Malformed("RTSP Digest challenge omitted nonce".into()))?;
    if parameters
        .get("qop")
        .is_some_and(|qop| !qop.split(',').any(|value| value.trim() == "auth"))
    {
        return Err(ProbeError::Unsupported(
            "RTSP Digest qop=auth is required".into(),
        ));
    }
    Ok(RtspDigestChallenge {
        realm,
        nonce,
        opaque: parameters.get("opaque").cloned(),
    })
}

fn rtsp_digest_authorization(
    credentials: &Credentials,
    challenge: &RtspDigestChallenge,
    method: &str,
    target: &str,
    nonce_count: u32,
) -> String {
    let mut random = [0_u8; 16];
    rand::rng().fill_bytes(&mut random);
    let cnonce = hex(&random);
    let nc = format!("{nonce_count:08x}");
    let ha1 = md5_hex(&format!(
        "{}:{}:{}",
        credentials.username, challenge.realm, credentials.password
    ));
    let ha2 = md5_hex(&format!("{method}:{target}"));
    let response = md5_hex(&format!(
        "{ha1}:{}:{nc}:{cnonce}:auth:{ha2}",
        challenge.nonce
    ));
    let mut authorization = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{}\", qop=auth, nc={nc}, cnonce=\"{cnonce}\"",
        credentials.username, challenge.realm, challenge.nonce, target, response
    );
    if let Some(opaque) = &challenge.opaque {
        authorization.push_str(&format!(", opaque=\"{opaque}\""));
    }
    authorization
}

fn parse_digest_parameters(value: &str) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    let mut remaining = value.trim();
    while !remaining.is_empty() {
        let Some((name, after_name)) = remaining.split_once('=') else {
            break;
        };
        let name = name.trim().to_ascii_lowercase();
        let after_name = after_name.trim_start();
        let (parsed, rest) = if let Some(after_quote) = after_name.strip_prefix('"') {
            let Some(end) = after_quote.find('"') else {
                break;
            };
            (after_quote[..end].to_owned(), &after_quote[end + 1..])
        } else {
            let (value, rest) = after_name.split_once(',').unwrap_or((after_name, ""));
            (value.to_owned(), rest)
        };
        result.insert(name, parsed.trim().to_owned());
        remaining = rest.strip_prefix(',').unwrap_or(rest).trim_start();
    }
    result
}

fn md5_hex(value: &str) -> String {
    let mut digest = Md5::new();
    digest.update(value.as_bytes());
    hex(&digest.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn classify_stream_error(message: &str, stage: ReplayStage) -> ProbeError {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("unauthorized") || normalized.contains("authentication") {
        ProbeError::Authentication
    } else if normalized.contains("not supported") || normalized.contains("unsupported") {
        ProbeError::Unsupported(
            "GStreamer reported an unsupported replay transport or codec".into(),
        )
    } else {
        // GStreamer errors can include the complete URI; do not forward arbitrary details.
        ProbeError::Media(format!("GStreamer failed during {}", stage.label()))
    }
}

fn fake_sink(
    observed: Arc<AtomicBool>,
    diagnostics: Arc<Mutex<Vec<ReplayDiagnostic>>>,
    started: Instant,
) -> Result<gst::Element, ProbeError> {
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("signal-handoffs", true)
        .build()
        .map_err(|error| ProbeError::Media(error.to_string()))?;
    sink.connect("handoff", false, move |_| {
        observed.store(true, Ordering::Release);
        record_stage(&diagnostics, started, ReplayStage::DecodedBuffer);
        None
    });
    Ok(sink)
}

fn record_stage(
    diagnostics: &Arc<Mutex<Vec<ReplayDiagnostic>>>,
    started: Instant,
    stage: ReplayStage,
) {
    let mut diagnostics = diagnostics
        .lock()
        .expect("replay diagnostics lock poisoned");
    if diagnostics.last().is_none_or(|last| last.stage != stage) {
        diagnostics.push(ReplayDiagnostic {
            stage,
            elapsed_ms: started.elapsed().as_millis(),
        });
    }
}

fn current_stage(diagnostics: &Arc<Mutex<Vec<ReplayDiagnostic>>>) -> ReplayStage {
    diagnostics
        .lock()
        .expect("replay diagnostics lock poisoned")
        .last()
        .map_or(ReplayStage::UriPrepared, |item| item.stage)
}

fn snapshot_diagnostics(diagnostics: &Arc<Mutex<Vec<ReplayDiagnostic>>>) -> Vec<ReplayDiagnostic> {
    diagnostics
        .lock()
        .expect("replay diagnostics lock poisoned")
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn preserves_replay_query_when_adding_rtsp_credentials() {
        let url = Url::parse("rtsp://camera.test/cam/playback?opaque=value&session=token").unwrap();
        let credentials = Credentials {
            username: "operator".into(),
            password: "secret".into(),
        };
        let playback = playback_uri(&url, Some(&credentials)).unwrap();
        assert_eq!(playback.query(), url.query());
        assert!(playback.as_str().contains("?opaque=value&session=token"));
    }

    #[test]
    fn formats_rtsp_digest_with_query_in_request_target() {
        let target =
            request_target(&Url::parse("rtsp://camera.test/cam/playback?session=value").unwrap());
        assert_eq!(target, "/cam/playback?session=value");
        let challenge = RtspDigestChallenge {
            realm: "nvr".into(),
            nonce: "nonce".into(),
            opaque: None,
        };
        let credentials = Credentials {
            username: "operator".into(),
            password: "secret".into(),
        };
        let authorization =
            rtsp_digest_authorization(&credentials, &challenge, "DESCRIBE", &target, 2);
        assert!(authorization.contains("uri=\"/cam/playback?session=value\""));
        assert!(!authorization.contains("secret"));
    }

    #[test]
    fn classifies_replay_errors_without_exposing_a_uri() {
        assert!(matches!(
            classify_stream_error("Unauthorized", ReplayStage::GstreamerPreroll),
            ProbeError::Authentication
        ));
        assert!(matches!(
            classify_stream_error("transport not supported", ReplayStage::GstreamerPreroll),
            ProbeError::Unsupported(_)
        ));
        assert!(matches!(
            classify_stream_error(
                "connection reset rtsp://operator:secret@camera",
                ReplayStage::GstreamerPreroll
            ),
            ProbeError::Media(message) if !message.contains("secret")
        ));
    }

    #[test]
    fn preflight_negotiates_digest_and_preserves_the_replay_query() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let first = read_request(&mut stream);
            assert!(first.starts_with("OPTIONS /cam/playback?opaque=session RTSP/1.0"));
            assert!(!first.contains("Authorization:"));
            stream
                .write_all(b"RTSP/1.0 401 Unauthorized\r\nCSeq: 1\r\nWWW-Authenticate: Digest realm=\"NVR\", nonce=\"nonce\", qop=\"auth\"\r\n\r\n")
                .unwrap();
            let retry = read_request(&mut stream);
            assert!(retry.starts_with("OPTIONS /cam/playback?opaque=session RTSP/1.0"));
            assert!(retry.contains("Authorization: Digest "));
            assert!(!retry.contains("secret"));
            stream
                .write_all(b"RTSP/1.0 200 OK\r\nCSeq: 2\r\n\r\n")
                .unwrap();
            let describe = read_request(&mut stream);
            assert!(describe.starts_with("DESCRIBE /cam/playback?opaque=session RTSP/1.0"));
            assert!(describe.contains("Authorization: Digest "));
            stream
                .write_all(b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let url = Url::parse(&format!(
            "rtsp://127.0.0.1:{port}/cam/playback?opaque=session"
        ))
        .unwrap();
        let credentials = Credentials {
            username: "operator".into(),
            password: "secret".into(),
        };
        let started = Instant::now();
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        rtsp_preflight(
            &url,
            Some(&credentials),
            Duration::from_secs(2),
            started,
            &diagnostics,
        )
        .unwrap();
        assert_eq!(
            snapshot_diagnostics(&diagnostics)
                .last()
                .map(|item| item.stage),
            Some(ReplayStage::RtspDescribeCompleted)
        );
        server.join().unwrap();
    }

    #[test]
    fn preflight_timeout_names_the_stalled_rtsp_operation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_request(&mut stream);
            thread::sleep(Duration::from_millis(150));
        });
        let url = Url::parse(&format!("rtsp://127.0.0.1:{port}/cam/playback")).unwrap();
        let started = Instant::now();
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let error = rtsp_preflight(&url, None, Duration::from_millis(30), started, &diagnostics)
            .unwrap_err();
        assert!(matches!(
            error,
            ProbeError::OperationTimeout { operation, .. } if operation == "RTSP replay RTSP OPTIONS"
        ));
        server.join().unwrap();
    }

    #[test]
    fn classifies_rtsp_503_as_a_server_response() {
        assert!(matches!(
            rtsp_status_error("DESCRIBE", 503),
            ProbeError::RtspServerResponse { operation, status }
                if operation == "DESCRIBE" && status == 503
        ));
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).unwrap();
            bytes.extend_from_slice(&chunk[..read]);
        }
        String::from_utf8(bytes).unwrap()
    }
}
