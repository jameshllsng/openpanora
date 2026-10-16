//! GTK-owned GStreamer live-view session.
//!
//! Network discovery happens elsewhere; only the final validated RTSP URI
//! reaches this module. It never writes a URI or credentials to diagnostics.

use device_probe::auth::Credentials;
use gst::prelude::*;
use gstreamer as gst;
use gtk::prelude::*;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEvent {
    Buffering,
    Playing,
    EndOfStream,
    Error,
}

pub struct LiveSession {
    pipeline: Option<gst::Element>,
    picture: gtk::Picture,
}

impl LiveSession {
    pub fn new(picture: &gtk::Picture) -> Result<Self, String> {
        gst::init().map_err(|_| "GStreamer initialization failed".to_owned())?;
        picture.set_can_shrink(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        Ok(Self {
            pipeline: None,
            picture: picture.clone(),
        })
    }

    pub fn start_rtsp(&mut self, uri: &Url, credentials: &Credentials) -> Result<(), String> {
        if !matches!(uri.scheme(), "rtsp" | "rtsps") {
            return Err("the selected stream did not return an RTSP URI".into());
        }
        let mut authenticated = uri.clone();
        authenticated
            .set_username(&credentials.username)
            .map_err(|_| "RTSP username cannot be represented safely".to_owned())?;
        authenticated
            .set_password(Some(&credentials.password))
            .map_err(|_| "RTSP password cannot be represented safely".to_owned())?;
        self.stop();
        let sink = self.paintable_sink()?;
        let playbin = gst::ElementFactory::make("playbin")
            .property("uri", authenticated.as_str())
            .property("video-sink", &sink)
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
        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([&source, &convert, &sink])
            .map_err(|_| "could not assemble synthetic video pipeline".to_owned())?;
        gst::Element::link_many([&source, &convert, &sink])
            .map_err(|_| "could not link synthetic video pipeline".to_owned())?;
        self.replace_pipeline(pipeline.upcast())
    }

    pub fn stop(&mut self) {
        if let Some(pipeline) = self.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.picture.set_paintable(None::<&gtk::gdk::Paintable>);
    }

    pub fn drain_events(&self) -> Vec<LiveEvent> {
        let Some(bus) = self.pipeline.as_ref().and_then(gst::Element::bus) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Some(message) = bus.pop_filtered(&[
            gst::MessageType::Error,
            gst::MessageType::Eos,
            gst::MessageType::Buffering,
            gst::MessageType::StateChanged,
        ]) {
            match message.view() {
                gst::MessageView::Error(_) => events.push(LiveEvent::Error),
                gst::MessageView::Eos(..) => events.push(LiveEvent::EndOfStream),
                gst::MessageView::Buffering(..) => events.push(LiveEvent::Buffering),
                gst::MessageView::StateChanged(state) if state.current() == gst::State::Playing => {
                    events.push(LiveEvent::Playing)
                }
                _ => {}
            }
        }
        events
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

    fn replace_pipeline(&mut self, pipeline: gst::Element) -> Result<(), String> {
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| "GStreamer could not start the media pipeline".to_owned())?;
        self.pipeline = Some(pipeline);
        Ok(())
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
    fn synthetic_pipeline_can_be_assembled() {
        gst::init().unwrap();
        let source = gst::ElementFactory::make("videotestsrc").build().unwrap();
        let sink = gst::ElementFactory::make("fakesink").build().unwrap();
        let pipeline = gst::Pipeline::new();
        pipeline.add_many([&source, &sink]).unwrap();
        source.link(&sink).unwrap();
        assert!(pipeline.set_state(gst::State::Playing).is_ok());
        let _ = pipeline.set_state(gst::State::Null);
    }
}
