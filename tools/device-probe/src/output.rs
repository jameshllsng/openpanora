use crate::{
    onvif::ProbeError,
    recordings::{
        RecordingInfo, RecordingSearchReport, RecordingSummary, ReplayConfiguration, ReplayUri,
    },
    rtsp::RtspTestResult,
};
use openpanora_core::CompatibilityReport;

pub fn print_report(report: &CompatibilityReport, json: bool) -> Result<(), ProbeError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|error| ProbeError::Output(error.to_string()))?
        );
        return Ok(());
    }

    println!("ONVIF compatibility report");
    if let Some(device) = &report.device {
        println!(
            "  Device: {} {} (firmware {})",
            device
                .manufacturer
                .as_deref()
                .unwrap_or("unknown manufacturer"),
            device.model.as_deref().unwrap_or("unknown model"),
            device.firmware.as_deref().unwrap_or("unknown")
        );
        println!(
            "  Identifiers: serial reported={}, hardware ID reported={} (values redacted)",
            device.serial_reported, device.hardware_id_reported
        );
    } else {
        println!("  Device information: unavailable");
    }
    println!("  Services discovered: {}", report.services.len());
    println!(
        "  Media profiles discovered: {}",
        report.media_profiles.len()
    );
    println!("  Capabilities:");
    for capability in &report.capabilities {
        println!(
            "    - {:?}: {:?} ({})",
            capability.kind, capability.status, capability.evidence
        );
    }
    Ok(())
}

pub fn print_rtsp(result: &RtspTestResult, json: bool) -> Result<(), ProbeError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(result)
                .map_err(|error| ProbeError::Output(error.to_string()))?
        );
    } else {
        println!("RTSP stream decoded successfully with GStreamer");
        println!("  Buffer observed: {}", result.decoded_buffer_observed);
        println!("  Elapsed: {} ms", result.elapsed_ms);
    }
    Ok(())
}

pub fn print_recording_summary(summary: &RecordingSummary, json: bool) -> Result<(), ProbeError> {
    print_json_or(json, summary, || {
        println!("Recording summary");
        println!("  Recordings: {}", summary.number_of_recordings);
        println!("  Data from: {}", display_time(summary.data_from));
        println!("  Data until: {}", display_time(summary.data_until));
    })
}

pub fn print_recordings(recordings: &[RecordingInfo], json: bool) -> Result<(), ProbeError> {
    print_json_or(json, recordings, || {
        println!("Recording configurations: {}", recordings.len());
        for recording in recordings {
            print_recording(recording);
        }
    })
}

pub fn print_recording_search(
    report: &RecordingSearchReport,
    json: bool,
) -> Result<(), ProbeError> {
    print_json_or(json, report, || {
        println!("Recording search completed: {}", report.complete);
        println!(
            "Interval filter applied: {}",
            report.interval_filter_applied
        );
        println!("Recording matches: {}", report.recordings.len());
        println!(
            "  Polls: {} (last state: {}; wait: {} ms; interval: {} ms)",
            report.polls_attempted,
            report.last_search_state,
            report.poll_wait_ms,
            report.poll_interval_ms
        );
        for recording in &report.recordings {
            print_recording(recording);
        }
    })
}

pub fn print_replay_uri(replay: &ReplayUri, json: bool) -> Result<(), ProbeError> {
    let display = replay.display();
    if json {
        println!(
            "{}",
            serde_json::json!({ "replay_uri": display, "note": "query parameters are redacted" })
        );
    } else {
        println!("Replay URI obtained (not playback-verified): {display}");
        println!("  Query parameters and embedded credentials are never displayed.");
    }
    Ok(())
}

pub fn print_replay_configuration(
    configuration: &ReplayConfiguration,
    json: bool,
) -> Result<(), ProbeError> {
    print_json_or(json, configuration, || {
        println!("Replay configuration");
        println!(
            "  Session timeout: {}",
            configuration
                .session_timeout
                .as_deref()
                .unwrap_or("not reported")
        );
    })
}

pub fn print_replay_test(
    replay: &ReplayUri,
    result: &RtspTestResult,
    json: bool,
) -> Result<(), ProbeError> {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "replay_uri": replay.display(),
                "query_parameters_redacted": true,
                "decoded_buffer_observed": result.decoded_buffer_observed,
                "elapsed_ms": result.elapsed_ms,
                "rtsp_transport": result.rtsp_transport,
                "onvif_mode_enabled": result.onvif_mode_enabled,
                "replay_query_preserved": result.replay_query_preserved,
                "diagnostics": result.diagnostics,
            })
        );
    } else {
        println!("Replay stream decoded successfully with GStreamer");
        println!("  URI: {}", replay.display());
        println!("  Buffer observed: {}", result.decoded_buffer_observed);
        println!("  Elapsed: {} ms", result.elapsed_ms);
        println!("  RTSP transport: {}", result.rtsp_transport);
        println!("  ONVIF replay mode: {}", result.onvif_mode_enabled);
        println!(
            "  Replay query preserved: {}",
            result.replay_query_preserved
        );
        println!("  Stages:");
        for stage in &result.diagnostics {
            println!("    - {:?}: {} ms", stage.stage, stage.elapsed_ms);
        }
    }
    Ok(())
}

fn print_recording(recording: &RecordingInfo) {
    println!(
        "  - token={} tracks={} from={} until={}",
        redact_token(&recording.token),
        recording.track_count,
        display_time(recording.earliest_recording),
        display_time(recording.latest_recording),
    );
}

fn redact_token(token: &str) -> String {
    let length = token.chars().count();
    if length <= 8 {
        "<redacted>".into()
    } else {
        let prefix: String = token.chars().take(4).collect();
        let suffix: String = token.chars().skip(length - 4).collect();
        format!("{prefix}…{suffix}")
    }
}

fn display_time(value: Option<chrono::DateTime<chrono::Utc>>) -> String {
    value.map_or_else(|| "unknown".into(), |time| time.to_rfc3339())
}

fn print_json_or<T: serde::Serialize + ?Sized>(
    json: bool,
    value: &T,
    human: impl FnOnce(),
) -> Result<(), ProbeError> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value)
                .map_err(|error| ProbeError::Output(error.to_string()))?
        );
    } else {
        human();
    }
    Ok(())
}
