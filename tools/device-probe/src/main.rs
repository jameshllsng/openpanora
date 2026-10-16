mod cli;
mod credentials;
mod onvif;
mod output;
mod recordings;
mod rtsp;

use clap::Parser;
use cli::{Cli, Command, RecordingsCommand};
use onvif::{HttpSoapTransport, OnvifClient, ProbeError};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("device-probe: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<(), ProbeError> {
    let timeout = cli.timeout();
    let json = cli.json;
    match cli.command {
        Command::Inspect(args) | Command::Capabilities(args) => {
            let credentials = credentials::load(&args)?;
            let endpoint = args.device_service_url()?;
            let transport = HttpSoapTransport::new(
                timeout,
                credentials.clone(),
                args.insecure_tls,
                args.tls_ca_file.as_deref(),
            )?;
            let client = OnvifClient::new(
                transport,
                endpoint,
                credentials,
                args.allowed_service_host()?,
            );
            let report = client.inspect().await?;
            output::print_report(&report, json)?;
        }
        Command::Recordings(args) => {
            let credentials = credentials::load(&args.device)?;
            let endpoint = args.device.device_service_url()?;
            let transport = HttpSoapTransport::new(
                timeout,
                credentials.clone(),
                args.device.insecure_tls,
                args.device.tls_ca_file.as_deref(),
            )?;
            let client = OnvifClient::new(
                transport,
                endpoint,
                credentials.clone(),
                args.device.allowed_service_host()?,
            );
            let recordings = recordings::RecordingClient::new(&client, timeout);
            match args.command {
                RecordingsCommand::Summary => {
                    output::print_recording_summary(&recordings.summary().await?, json)?;
                }
                RecordingsCommand::List => {
                    output::print_recordings(&recordings.list().await?, json)?;
                }
                RecordingsCommand::Search(args) => {
                    output::print_recording_search(
                        &recordings.search(args.start, args.end, args.limit).await?,
                        json,
                    )?;
                }
                RecordingsCommand::ReplayUri(args) => {
                    output::print_replay_uri(
                        &recordings.replay_uri(&args.recording_token).await?,
                        json,
                    )?;
                }
                RecordingsCommand::ReplayConfiguration => {
                    output::print_replay_configuration(
                        &recordings.replay_configuration().await?,
                        json,
                    )?;
                }
                RecordingsCommand::ReplayTest(args) => {
                    let replay = recordings.replay_uri(&args.replay.recording_token).await?;
                    let result = rtsp::test_replay_with_credentials(
                        replay.url(),
                        credentials.as_ref(),
                        rtsp::ReplayTestOptions::new(timeout, args.start.is_some()),
                    )?;
                    output::print_replay_test(&replay, &result, json)?;
                }
            }
        }
        Command::RtspTest(args) => {
            let result = rtsp::test(&args.url, timeout)?;
            output::print_rtsp(&result, json)?;
        }
    }
    Ok(())
}
