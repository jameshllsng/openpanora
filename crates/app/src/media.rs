//! GTK-owned GStreamer live-view session with privacy-safe observability.

use device_probe::auth::Credentials;
use gst::prelude::*;
use gstreamer as gst;
use gtk::prelude::*;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use url::Url;

const STARTUP_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_AUTOMATIC_RECONNECTS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveState {
    Disconnected,
    Connecting,
    Buffering,
    ReceivingVideo,
    Stalled,
    Error,
    Reconnecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Transient,
    Permanent,
}

#[derive(Debug, Default)]
pub struct ReconnectPlan {
    attempts: u8,
    pending: bool,
}

impl ReconnectPlan {
    pub fn schedule(&mut self, transient: bool) -> Option<(u8, Duration)> {
        if !transient || self.pending {
            return None;
        }
        let attempt = self.attempts.saturating_add(1);
        let delay = reconnect_delay(attempt)?;
        self.attempts = attempt;
        self.pending = true;
        Some((attempt, delay))
    }
    pub fn retry_fired(&mut self) {
        self.pending = false;
    }
    pub fn pending(&self) -> bool {
        self.pending
    }
    pub fn cancel(&mut self) {
        self.attempts = 0;
        self.pending = false;
    }
    pub fn recovered(&mut self) {
        self.cancel();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveDiagnostics {
    pub state: LiveState,
    pub decoded_frames: u64,
    pub last_frame_age: Option<Duration>,
    pub startup_timed_out: bool,
    pub failure: Option<FailureKind>,
}

#[derive(Default)]
struct FrameMetrics {
    decoded_frames: u64,
    last_frame: Option<Instant>,
}

pub struct LiveSession {
    pipeline: Option<gst::Element>,
    picture: gtk::Picture,
    metrics: Arc<Mutex<FrameMetrics>>,
    started: Instant,
    state: LiveState,
    failure: Option<FailureKind>,
    restart: Option<(Url, Credentials)>,
}

impl LiveSession {
    pub fn new(picture: &gtk::Picture) -> Result<Self, String> {
        gst::init().map_err(|_| "GStreamer initialization failed".to_owned())?;
        picture.set_can_shrink(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        Ok(Self {
            pipeline: None,
            picture: picture.clone(),
            metrics: Arc::new(Mutex::new(FrameMetrics::default())),
            started: Instant::now(),
            state: LiveState::Disconnected,
            failure: None,
            restart: None,
        })
    }

    pub fn start_rtsp(&mut self, uri: &Url, credentials: &Credentials) -> Result<(), String> {
        if !matches!(uri.scheme(), "rtsp" | "rtsps") {
            return Err("the selected stream did not return an RTSP URI".into());
        }
        self.stop();
        let mut authenticated = uri.clone();
        authenticated
            .set_username(&credentials.username)
            .map_err(|_| "RTSP username cannot be represented safely".to_owned())?;
        authenticated
            .set_password(Some(&credentials.password))
            .map_err(|_| "RTSP password cannot be represented safely".to_owned())?;
        self.restart = Some((uri.clone(), credentials.clone()));
        let sink = self.paintable_sink()?;
        let filter = self.frame_probe_filter()?;
        let playbin = gst::ElementFactory::make("playbin")
            .property("uri", authenticated.as_str())
            .property("video-sink", &sink)
            .property("video-filter", &filter)
            .build()
            .map_err(|_| "GStreamer playbin is unavailable".to_owned())?;
        // TCP avoids common UDP/NAT failures. GStreamer performs RTSP Digest.
        playbin.connect("source-setup", false, move |values| {
            if let Ok(source) = values[1].get::<gst::Element>() {
                if source.find_property("protocols").is_some() {
                    source.set_property_from_str("protocols", "tcp");
                }
            }
            None
        });
        self.replace_pipeline(playbin)
    }

    pub fn start_test_pattern(&mut self) -> Result<(), String> {
        self.stop();
        let sink = self.paintable_sink()?;
        let source = gst::ElementFactory::make("videotestsrc")
            .property("is-live", true)
            .build()
            .map_err(|_| "GStreamer videotestsrc is unavailable".to_owned())?;
        let convert = gst::ElementFactory::make("videoconvert")
            .build()
            .map_err(|_| "GStreamer videoconvert is unavailable".to_owned())?;
        let filter = self.frame_probe_filter()?;
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([&source, &convert, &filter, &sink])
            .map_err(|_| "could not assemble synthetic video pipeline".to_owned())?;
        gst::Element::link_many([&source, &convert, &filter, &sink])
            .map_err(|_| "could not link synthetic video pipeline".to_owned())?;
        self.replace_pipeline(pipeline.upcast())
    }

    pub fn stop(&mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.picture.set_paintable(None::<&gtk::gdk::Paintable>);
        self.state = LiveState::Disconnected;
        self.failure = None;
        self.restart = None;
    }

    pub fn reconnect(&mut self) -> Result<(), String> {
        let Some((uri, credentials)) = self.restart.clone() else {
            return Err("there is no reconnectable RTSP session".into());
        };
        self.restart = Some((uri.clone(), credentials.clone()));
        self.start_rtsp(&uri, &credentials)
    }

    pub fn mark_reconnecting(&mut self) {
        self.state = LiveState::Reconnecting;
    }

    /// Processes queued bus messages; no URI, credentials, or GStreamer debug text escapes.
    pub fn poll(&mut self) -> LiveDiagnostics {
        if let Some(bus) = self.pipeline.as_ref().and_then(gst::Element::bus) {
            while let Some(message) = bus.pop_filtered(&[
                gst::MessageType::Error,
                gst::MessageType::Eos,
                gst::MessageType::Buffering,
                gst::MessageType::StateChanged,
                gst::MessageType::AsyncDone,
            ]) {
                match message.view() {
                    gst::MessageView::Error(error) => {
                        self.failure = Some(classify_gstreamer_error(&error.error().to_string()));
                        self.state = LiveState::Error;
                    }
                    gst::MessageView::Eos(..) => self.state = LiveState::Stalled,
                    gst::MessageView::Buffering(_) => self.state = LiveState::Buffering,
                    gst::MessageView::StateChanged(state)
                        if state.current() >= gst::State::Paused
                            && self.state == LiveState::Connecting =>
                    {
                        self.state = LiveState::Buffering
                    }
                    _ => {}
                }
            }
        }
        let metrics = self.metrics.lock().expect("frame metrics mutex poisoned");
        let age = metrics.last_frame.map(|value| value.elapsed());
        let startup_timed_out =
            metrics.decoded_frames == 0 && self.started.elapsed() >= STARTUP_FRAME_TIMEOUT;
        if metrics.decoded_frames > 0 {
            self.state = LiveState::ReceivingVideo;
            self.failure = None;
        } else if startup_timed_out
            && !matches!(self.state, LiveState::Error | LiveState::Disconnected)
        {
            self.state = LiveState::Stalled;
        }
        LiveDiagnostics {
            state: self.state,
            decoded_frames: metrics.decoded_frames,
            last_frame_age: age,
            startup_timed_out,
            failure: self.failure,
        }
    }

    fn paintable_sink(&self) -> Result<gst::Element, String> {
        let sink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| {
                "gtk4paintablesink is unavailable; install gst-plugin-gtk4 and set GST_PLUGIN_PATH"
                    .to_owned()
            })?;
        let paintable = sink.property::<gtk::gdk::Paintable>("paintable");
        self.picture.set_paintable(Some(&paintable));
        Ok(sink)
    }

    fn frame_probe_filter(&self) -> Result<gst::Element, String> {
        let filter = gst::ElementFactory::make("identity")
            .build()
            .map_err(|_| "GStreamer identity element is unavailable".to_owned())?;
        let metrics = self.metrics.clone();
        filter
            .static_pad("src")
            .ok_or_else(|| "identity element has no source pad".to_owned())?
            .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if info.buffer().is_some() {
                    let mut metrics = metrics.lock().expect("frame metrics mutex poisoned");
                    metrics.decoded_frames += 1;
                    metrics.last_frame = Some(Instant::now());
                }
                gst::PadProbeReturn::Ok
            });
        Ok(filter)
    }

    fn replace_pipeline(&mut self, pipeline: gst::Element) -> Result<(), String> {
        self.started = Instant::now();
        *self.metrics.lock().expect("frame metrics mutex poisoned") = FrameMetrics::default();
        self.state = LiveState::Connecting;
        self.failure = None;
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| "GStreamer could not start the media pipeline".to_owned())?;
        self.pipeline = Some(pipeline);
        Ok(())
    }
}

pub fn reconnect_delay(attempt: u8) -> Option<Duration> {
    match attempt {
        1 => Some(Duration::from_secs(1)),
        2 => Some(Duration::from_secs(2)),
        3 => Some(Duration::from_secs(4)),
        _ => None,
    }
}

fn classify_gstreamer_error(text: &str) -> FailureKind {
    let text = text.to_ascii_lowercase();
    if [
        "unauthorized",
        "not authorized",
        "authentication",
        "401",
        "403",
        "missing plugin",
        "no decoder",
        "not-negotiated",
        "unsupported",
    ]
    .iter()
    .any(|marker| text.contains(marker))
    {
        FailureKind::Permanent
    } else {
        FailureKind::Transient
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_pipeline_delivers_buffers() {
        gst::init().unwrap();
        let metrics = Arc::new(Mutex::new(FrameMetrics::default()));
        let source = gst::ElementFactory::make("videotestsrc")
            .property("num-buffers", 3i32)
            .build()
            .unwrap();
        let filter = gst::ElementFactory::make("identity").build().unwrap();
        let seen = metrics.clone();
        filter
            .static_pad("src")
            .unwrap()
            .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
                seen.lock().unwrap().decoded_frames += 1;
                gst::PadProbeReturn::Ok
            });
        let sink = gst::ElementFactory::make("fakesink").build().unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline.add_many([&source, &filter, &sink]).unwrap();
        gst::Element::link_many([&source, &filter, &sink]).unwrap();
        pipeline.set_state(gst::State::Playing).unwrap();
        pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(2), &[gst::MessageType::Eos]);
        assert_eq!(metrics.lock().unwrap().decoded_frames, 3);
        let _ = pipeline.set_state(gst::State::Null);
    }

    #[test]
    fn retry_policy_is_bounded_and_exponential() {
        assert_eq!(reconnect_delay(1), Some(Duration::from_secs(1)));
        assert_eq!(reconnect_delay(2), Some(Duration::from_secs(2)));
        assert_eq!(reconnect_delay(3), Some(Duration::from_secs(4)));
        assert_eq!(reconnect_delay(4), None);
    }

    #[test]
    fn permanent_error_classification_does_not_retry() {
        assert_eq!(
            classify_gstreamer_error("Unauthorized"),
            FailureKind::Permanent
        );
        assert_eq!(
            classify_gstreamer_error("missing plugin"),
            FailureKind::Permanent
        );
        assert_eq!(
            classify_gstreamer_error("connection timed out"),
            FailureKind::Transient
        );
    }

    #[test]
    fn reconnect_plan_is_cancelable_and_resets_after_recovery() {
        let mut plan = ReconnectPlan::default();
        assert_eq!(plan.schedule(true).map(|value| value.0), Some(1));
        assert!(plan.schedule(true).is_none());
        plan.cancel();
        assert_eq!(plan.schedule(true).map(|value| value.0), Some(1));
        plan.retry_fired();
        assert_eq!(plan.schedule(true).map(|value| value.0), Some(2));
        plan.recovered();
        assert_eq!(plan.schedule(true).map(|value| value.0), Some(1));
        assert!(ReconnectPlan::default().schedule(false).is_none());
    }
}
