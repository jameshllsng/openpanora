use crate::onvif::ProbeError;
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
    gst::init().map_err(|error| ProbeError::Media(error.to_string()))?;

    let observed = Arc::new(AtomicBool::new(false));
    let video_sink = fake_sink(observed.clone())?;
    let audio_sink = fake_sink(observed.clone())?;
    let playbin = gst::ElementFactory::make("playbin")
        .property("uri", url.as_str())
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
                    let message = error.error().to_string();
                    if message.to_ascii_lowercase().contains("unauthorized") {
                        break Err(ProbeError::Authentication);
                    }
                    break Err(ProbeError::Media(message));
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
