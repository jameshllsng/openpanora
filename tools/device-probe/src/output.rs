use crate::{onvif::ProbeError, rtsp::RtspTestResult};
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
