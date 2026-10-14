use crate::{credentials::Credentials, onvif::ProbeError};
use gst::prelude::*;
use gstreamer as gst;
use serde::Serialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;

#[derive(Debug, Serialize)]
pub struct RtspTestResult {
    pub decoded_buffer_observed: bool,
    pub elapsed_ms: u128,
}

pub fn test(url: &Url, timeout: Duration) -> Result<RtspTestResult, ProbeError> {
    test_internal(url, None, timeout)
}

pub fn test_with_credentials(
    url: &Url,
    credentials: Option<&Credentials>,
    timeout: Duration,
) -> Result<RtspTestResult, ProbeError> {
    test_internal(url, credentials, timeout)
}

fn test_internal(
    url: &Url,
    credentials: Option<&Credentials>,
    timeout: Duration,
) -> Result<RtspTestResult, ProbeError> {
    gst::init().map_err(|error| ProbeError::Media(error.to_string()))?;

    let mut playback_uri = url.clone();
    if let Some(credentials) = credentials {
        playback_uri
            .set_username(&credentials.username)
            .map_err(|_| ProbeError::Input("RTSP username cannot be represented safely".into()))?;
        playback_uri
            .set_password(Some(&credentials.password))
            .map_err(|_| ProbeError::Input("RTSP password cannot be represented safely".into()))?;
    }

    let observed = Arc::new(AtomicBool::new(false));
    let video_sink = fake_sink(observed.clone())?;
    let audio_sink = fake_sink(observed.clone())?;
    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", playback_uri.as_str())
        .property("video-sink", &video_sink)
        .property("audio-sink", &audio_sink)
        .build()
        .map_err(|error| ProbeError::Media(error.to_string()))?;
    let bus = playbin
        .bus()
        .ok_or_else(|| ProbeError::Media("GStreamer playbin has no message bus".into()))?;
    let started = Instant::now();
    playbin
        .set_state(gst::State::Playing)
        .map_err(|error| ProbeError::Media(error.to_string()))?;

    let outcome = loop {
        if observed.load(Ordering::Acquire) {
            break Ok(RtspTestResult {
                decoded_buffer_observed: true,
                elapsed_ms: started.elapsed().as_millis(),
            });
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break Err(ProbeError::Timeout);
        }
        let wait = remaining.min(Duration::from_millis(250));
        if let Some(message) = bus.timed_pop(gst::ClockTime::from_nseconds(
            wait.as_nanos().min(u64::MAX as u128) as u64,
        )) {
            match message.view() {
                gst::MessageView::Error(error) => {
                    break Err(classify_stream_error(&error.error().to_string()));
                }
                gst::MessageView::Eos(_) if !observed.load(Ordering::Acquire) => {
                    break Err(ProbeError::Media(
                        "stream ended before GStreamer decoded a media buffer".into(),
                    ));
                }
                _ => {}
            }
        }
    };

    let _ = playbin.set_state(gst::State::Null);
    outcome
}

fn classify_stream_error(message: &str) -> ProbeError {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("unauthorized") || normalized.contains("authentication") {
        ProbeError::Authentication
    } else if normalized.contains("not supported") || normalized.contains("unsupported") {
        ProbeError::Unsupported(
            "GStreamer reported an unsupported replay transport or codec".into(),
        )
    } else {
        ProbeError::Media(message.into())
    }
}

fn fake_sink(observed: Arc<AtomicBool>) -> Result<gst::Element, ProbeError> {
    let sink = gst::ElementFactory::make("fakesink")
        .property("sync", false)
        .property("signal-handoffs", true)
        .build()
        .map_err(|error| ProbeError::Media(error.to_string()))?;
    sink.connect("handoff", false, move |_| {
        observed.store(true, Ordering::Release);
        None
    });
    Ok(sink)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_replay_stream_errors_without_a_pipeline() {
        assert!(matches!(
            classify_stream_error("Unauthorized"),
            ProbeError::Authentication
        ));
        assert!(matches!(
            classify_stream_error("transport not supported"),
            ProbeError::Unsupported(_)
        ));
        assert!(matches!(
            classify_stream_error("connection reset"),
            ProbeError::Media(_)
        ));
    }
}
