use crate::{auth::Credentials, cli::DeviceArgs, onvif::ProbeError};
use std::io::{self, IsTerminal};

pub fn load(args: &DeviceArgs) -> Result<Option<Credentials>, ProbeError> {
    let Some(username) = args.username.as_deref() else {
        return Ok(None);
    };
    if username.is_empty() {
        return Err(ProbeError::Input("username cannot be empty".into()));
    }

    let password = if args.password_stdin {
        let mut secret = String::new();
        io::stdin()
            .read_line(&mut secret)
            .map_err(|error| ProbeError::Credential(error.to_string()))?;
        secret.trim_end_matches(['\r', '\n']).to_owned()
    } else if args.keyring {
        let account = format!("{}:{username}", args.credential_scope()?);
        keyring::Entry::new("org.openpanora.Device", &account)
            .map_err(|error| ProbeError::Credential(error.to_string()))?
            .get_password()
            .map_err(|error| ProbeError::Credential(error.to_string()))?
    } else if io::stdin().is_terminal() {
        rpassword::prompt_password(format!("Password for {username}: "))
            .map_err(|error| ProbeError::Credential(error.to_string()))?
    } else {
        return Err(ProbeError::Input(
            "a username in non-interactive mode requires --password-stdin or --keyring".into(),
        ));
    };

    Ok(Some(Credentials {
        username: username.to_owned(),
        password,
    }))
}
