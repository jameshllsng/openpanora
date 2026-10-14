mod cli;
mod credentials;
mod onvif;
mod output;
mod rtsp;

use clap::Parser;
use cli::{Cli, Command};
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
            let transport = HttpSoapTransport::new(timeout)?;
            let client = OnvifClient::new(transport, endpoint, credentials);
            let report = client.inspect().await?;
            output::print_report(&report, json)?;
        }
        Command::RtspTest(args) => {
            let result = rtsp::test(&args.url, timeout)?;
            output::print_rtsp(&result, json)?;
        }
    }
    Ok(())
}
