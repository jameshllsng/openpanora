#![allow(deprecated)]
mod media;

use device_probe::{
    auth::Credentials,
    onvif::{HttpSoapTransport, OnvifClient},
};
use gtk::prelude::*;
use openpanora_core::{
    CompatibilityReport, Device, DeviceRepository, MediaProfile, SqliteDeviceRepository,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc,
    time::Duration,
};
use url::Url;

const APP_ID: &str = "org.openpanora.OpenPanora";
const KEYRING_SERVICE: &str = "org.openpanora.Device";

fn main() -> gtk::glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &gtk::Application) {
    let repository = match open_repository() {
        Ok(value) => Rc::new(value),
        Err(error) => {
            eprintln!("OpenPanora database unavailable: {error}");
            return;
        }
    };
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("OpenPanora")
        .default_width(1180)
        .default_height(760)
        .build();
    let stack = gtk::Stack::builder().hexpand(true).vexpand(true).build();
    stack.add_titled(&live_view(repository.clone()), Some("live"), "Live View");
    stack.add_titled(
        &devices_view(repository, &window),
        Some("devices"),
        "Devices",
    );
    stack.add_titled(&recordings_view(), Some("recordings"), "Recordings");
    stack.add_titled(&diagnostics_view(), Some("diagnostics"), "Diagnostics");
    stack.add_titled(
        &page(
            "Settings",
            "Passwords are held by Linux Secret Service; they are never stored in SQLite.",
        ),
        Some("settings"),
        "Settings",
    );
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(
        &gtk::StackSidebar::builder()
            .stack(&stack)
            .width_request(190)
            .build(),
    );
    content.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    content.append(&stack);
    window.set_child(Some(&content));
    window.present();
}

fn open_repository() -> Result<SqliteDeviceRepository, String> {
    let directory = gtk::glib::user_data_dir().join("openpanora");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    SqliteDeviceRepository::open(directory.join("openpanora.sqlite3"))
        .map_err(|error| error.to_string())
}

fn page(title: &str, subtitle: &str) -> gtk::Box {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(28)
        .margin_end(28)
        .build();
    content.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["title-1"])
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label(subtitle)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    content
}

fn devices_view(
    repository: Rc<SqliteDeviceRepository>,
    parent: &gtk::ApplicationWindow,
) -> gtk::Box {
    let view = page(
        "Devices",
        "Registered endpoints persist locally. Connection tests are explicit and never scan a network.",
    );
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let add = gtk::Button::with_label("Add device");
    view.append(&add);
    view.append(&list);
    refresh(&list, repository.clone(), parent);
    let parent = parent.clone();
    let list_copy = list.clone();
    add.connect_clicked(move |_| dialog(&parent, repository.clone(), None, list_copy.clone()));
    view
}

fn refresh(
    list: &gtk::ListBox,
    repository: Rc<SqliteDeviceRepository>,
    parent: &gtk::ApplicationWindow,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    match repository.list() {
        Ok(items) if items.is_empty() => list.append(&row(
            "No registered devices",
            "Add an explicit ONVIF service URL.",
        )),
        Ok(items) => {
            for item in items {
                list.append(&device_row(item, repository.clone(), parent, list));
            }
        }
        Err(error) => list.append(&row("Database error", &error.to_string())),
    }
}

fn device_row(
    device: Device,
    repository: Rc<SqliteDeviceRepository>,
    parent: &gtk::ApplicationWindow,
    list: &gtk::ListBox,
) -> gtk::ListBoxRow {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .build();
    box_.append(
        &gtk::Label::builder()
            .label(&device.name)
            .halign(gtk::Align::Start)
            .build(),
    );
    box_.append(
        &gtk::Label::builder()
            .label(format!("{} · {:?}", device.host, device.kind))
            .halign(gtk::Align::Start)
            .css_classes(["dim-label"])
            .build(),
    );
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let connect = gtk::Button::with_label("Connect & inspect");
    let edit = gtk::Button::with_label("Edit");
    let remove = gtk::Button::with_label("Remove");
    let status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .build();
    actions.append(&connect);
    actions.append(&edit);
    actions.append(&remove);
    box_.append(&actions);
    box_.append(&status);
    let target = device.clone();
    connect.connect_clicked(move |_| inspect(target.clone(), status.clone()));
    let edit_parent = parent.clone();
    let edit_list = list.clone();
    let edit_device = device.clone();
    let edit_repository = repository.clone();
    edit.connect_clicked(move |_| {
        dialog(
            &edit_parent,
            edit_repository.clone(),
            Some(edit_device.clone()),
            edit_list.clone(),
        )
    });
    let id = device.id;
    let remove_list = list.clone();
    let remove_parent = parent.clone();
    let remove_repository = repository.clone();
    remove.connect_clicked(move |_| {
        if remove_repository.remove(id).unwrap_or(false) {
            refresh(&remove_list, remove_repository.clone(), &remove_parent);
        }
    });
    gtk::ListBoxRow::builder().child(&box_).build()
}

fn dialog(
    parent: &gtk::ApplicationWindow,
    repository: Rc<SqliteDeviceRepository>,
    existing: Option<Device>,
    list: gtk::ListBox,
) {
    let dialog = gtk::Dialog::builder()
        .transient_for(parent)
        .modal(true)
        .title("Add device")
        .build();
    dialog.add_button("Cancel", gtk::ResponseType::Cancel);
    dialog.add_button("Save", gtk::ResponseType::Accept);
    let grid = gtk::Grid::builder()
        .row_spacing(8)
        .column_spacing(8)
        .margin_top(14)
        .margin_bottom(14)
        .margin_start(14)
        .margin_end(14)
        .build();
    let name = gtk::Entry::new();
    let endpoint = gtk::Entry::new();
    let username = gtk::Entry::new();
    let password = gtk::PasswordEntry::new();
    let tls_ca_file = gtk::Entry::new();
    tls_ca_file.set_placeholder_text(Some("Optional PEM CA file"));
    let insecure_tls =
        gtk::CheckButton::with_label("Unsafe: accept invalid TLS certificate (diagnostics only)");
    if let Some(device) = &existing {
        name.set_text(&device.name);
        endpoint.set_text(&device.host);
        username.set_text(device.credential_key.as_deref().unwrap_or(""));
        tls_ca_file.set_text(device.tls_ca_file.as_deref().unwrap_or(""));
        insecure_tls.set_active(device.insecure_tls);
    }
    for (index, (label, field)) in [
        ("Display name", name.clone().upcast::<gtk::Widget>()),
        ("ONVIF service URL", endpoint.clone().upcast()),
        ("Username", username.clone().upcast()),
        ("Password (Secret Service)", password.clone().upcast()),
        ("Trusted CA PEM", tls_ca_file.clone().upcast()),
    ]
    .into_iter()
    .enumerate()
    {
        grid.attach(
            &gtk::Label::builder()
                .label(label)
                .halign(gtk::Align::End)
                .build(),
            0,
            index as i32,
            1,
            1,
        );
        grid.attach(&field, 1, index as i32, 1, 1);
    }
    grid.attach(&insecure_tls, 1, 5, 1, 1);
    dialog.content_area().append(&grid);
    let refresh_parent = parent.clone();
    dialog.connect_response(move |dialog, response| {
        if response == gtk::ResponseType::Accept {
            let Ok(url) = Url::parse(endpoint.text().as_str()) else {
                return;
            };
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return;
            }
            let mut device = existing.clone().unwrap_or_else(|| {
                Device::new(
                    name.text(),
                    endpoint.text(),
                    url.port_or_known_default().unwrap_or(80),
                )
            });
            device.name = name.text().to_string();
            device.host = endpoint.text().to_string();
            device.port = url.port_or_known_default().unwrap_or(80);
            device.credential_key =
                (!username.text().is_empty()).then(|| username.text().to_string());
            device.tls_ca_file =
                (!tls_ca_file.text().is_empty()).then(|| tls_ca_file.text().to_string());
            device.insecure_tls = insecure_tls.is_active();
            if let (Some(user), false) =
                (device.credential_key.as_deref(), password.text().is_empty())
            {
                let stored = keyring::Entry::new(KEYRING_SERVICE, &format!("{}:{user}", device.id))
                    .and_then(|entry| entry.set_password(password.text().as_str()));
                if stored.is_err() {
                    return;
                }
            }
            let _ = repository.save(&device);
            refresh(&list, repository.clone(), &refresh_parent);
        }
        dialog.close();
    });
    dialog.present();
}

fn inspect(device: Device, status: gtk::Label) {
    status.set_text("Connecting…");
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let result = inspect_blocking(device);
        let _ = send.send(result);
    });
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        match receive.try_recv() {
            Ok(Ok(message)) => {
                status.set_text(&message);
                gtk::glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                status.set_text(&format!("Connection failed: {error}"));
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => gtk::glib::ControlFlow::Break,
        }
    });
}

fn inspect_blocking(device: Device) -> Result<String, String> {
    let (client, _) = onvif_client(&device)?;
    let runtime = tokio::runtime::Runtime::new().map_err(|_| "async runtime unavailable")?;
    let report = runtime
        .block_on(client.inspect())
        .map_err(|error| error.to_string())?;
    format_verified_inspection(report)
}

fn onvif_client(device: &Device) -> Result<(OnvifClient<HttpSoapTransport>, Credentials), String> {
    let endpoint = if device.host.starts_with("http://") || device.host.starts_with("https://") {
        Url::parse(&device.host).map_err(|error| error.to_string())?
    } else {
        Url::parse(&format!(
            "https://{}:{}/onvif/device_service",
            device.host, device.port
        ))
        .map_err(|error| error.to_string())?
    };
    let username = device
        .credential_key
        .clone()
        .ok_or("no credential reference")?;
    let password = keyring::Entry::new(KEYRING_SERVICE, &format!("{}:{username}", device.id))
        .map_err(|_| "Secret Service unavailable")?
        .get_password()
        .map_err(|_| "credential unavailable")?;
    let transport = HttpSoapTransport::new(
        Duration::from_secs(10),
        Some(Credentials {
            username: username.clone(),
            password: password.clone(),
        }),
        device.insecure_tls,
        device.tls_ca_file.as_deref().map(std::path::Path::new),
    )
    .map_err(|error| error.to_string())?;
    Ok((
        OnvifClient::new(transport, endpoint, None, None),
        Credentials { username, password },
    ))
}

fn format_verified_inspection(report: CompatibilityReport) -> Result<String, String> {
    let device_information = report.device.ok_or_else(|| {
        "device identification was not verified; refusing to report a successful connection"
            .to_owned()
    })?;
    if report.services.is_empty() {
        return Err("ONVIF service discovery returned no services".into());
    }
    if report.media_profiles.is_empty() {
        return Err("media profile discovery returned no profiles".into());
    }
    Ok(format!(
        "Verified: {} {} · firmware {} · {} service(s), {} media profile(s)",
        device_information
            .manufacturer
            .unwrap_or_else(|| "Unknown manufacturer".into()),
        device_information
            .model
            .unwrap_or_else(|| "unknown model".into()),
        device_information
            .firmware
            .unwrap_or_else(|| "unknown".into()),
        report.services.len(),
        report.media_profiles.len()
    ))
}

fn live_view(repository: Rc<SqliteDeviceRepository>) -> gtk::Box {
    let view = page(
        "Live View",
        "Select a registered device, discover its ONVIF profiles, then explicitly start one live stream.",
    );
    let devices = gtk::ComboBoxText::new();
    if let Ok(items) = repository.list() {
        for device in items {
            devices.append(Some(&device.id.to_string()), &device.name);
        }
    }
    let profiles = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    let profiles_scroll = gtk::ScrolledWindow::builder()
        .child(&profiles)
        .min_content_height(150)
        .max_content_height(220)
        .vexpand(false)
        .build();
    let profile_tokens = Rc::new(RefCell::new(Vec::<String>::new()));
    let discover = gtk::Button::with_label("Discover profiles");
    let selectors = gtk::Grid::builder()
        .column_spacing(8)
        .row_spacing(8)
        .build();
    selectors.attach(&gtk::Label::new(Some("Device")), 0, 0, 1, 1);
    selectors.attach(&devices, 1, 0, 1, 1);
    selectors.attach(&discover, 2, 0, 1, 1);
    selectors.attach(&gtk::Label::new(Some("Profile")), 0, 1, 1, 1);
    selectors.attach(&profiles_scroll, 1, 1, 2, 1);
    let picture = gtk::Picture::builder()
        .hexpand(true)
        .vexpand(true)
        .height_request(360)
        .build();
    let status = gtk::Label::new(Some("Disconnected"));
    let start = gtk::Button::with_label("Start live view");
    let retry = gtk::Button::with_label("Retry");
    let test = gtk::Button::with_label("Test video surface");
    let stop = gtk::Button::with_label("Stop");
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.append(&start);
    controls.append(&retry);
    controls.append(&test);
    controls.append(&stop);
    controls.append(&status);
    let session = Rc::new(RefCell::new(None::<media::LiveSession>));
    let generation = Rc::new(Cell::new(0_u64));
    let session_start = session.clone();
    let picture_start = picture.clone();
    let status_start = status.clone();
    let generation_test = generation.clone();
    test.connect_clicked(move |_| {
        generation_test.set(generation_test.get().wrapping_add(1));
        if let Some(mut old) = session_start.borrow_mut().take() {
            old.stop();
        }
        match media::LiveSession::new(&picture_start).and_then(|mut value| {
            value.start_test_pattern()?;
            Ok(value)
        }) {
            Ok(value) => {
                *session_start.borrow_mut() = Some(value);
                status_start.set_text("Synthetic pipeline started");
            }
            Err(error) => status_start.set_text(&format!("Error: {error}")),
        }
    });
    let repository_discover = repository.clone();
    let profiles_discover = profiles.clone();
    let profile_tokens_discover = profile_tokens.clone();
    let devices_discover = devices.clone();
    let status_discover = status.clone();
    discover.connect_clicked(move |_| {
        let Some(id) = devices_discover.active_id() else {
            status_discover.set_text("Select a registered device first");
            return;
        };
        let device = repository_discover
            .list()
            .ok()
            .and_then(|items| items.into_iter().find(|device| device.id.to_string() == id));
        let Some(device) = device else {
            status_discover.set_text("The selected device no longer exists");
            return;
        };
        status_discover.set_text("Discovering ONVIF profiles…");
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(live_profiles_blocking(device));
        });
        let profiles = profiles_discover.clone();
        let status = status_discover.clone();
        let profile_tokens = profile_tokens_discover.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            match receive.try_recv() {
                Ok(Ok(items)) => {
                    while let Some(child) = profiles.first_child() {
                        profiles.remove(&child);
                    }
                    profile_tokens.borrow_mut().clear();
                    for profile in items {
                        profile_tokens.borrow_mut().push(profile.token);
                        profiles.append(&row(
                            profile.name.as_deref().unwrap_or("Unnamed ONVIF profile"),
                            "ONVIF media profile",
                        ));
                    }
                    status.set_text("Profiles discovered. Select one and start live view.");
                    gtk::glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    status.set_text(&format!("Profile discovery failed: {error}"));
                    gtk::glib::ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    status.set_text("Profile discovery worker stopped unexpectedly");
                    gtk::glib::ControlFlow::Break
                }
            }
        });
    });
    let repository_start = repository.clone();
    let devices_start = devices.clone();
    let profiles_start = profiles.clone();
    let profile_tokens_start = profile_tokens.clone();
    let session_live = session.clone();
    let picture_live = picture.clone();
    let status_live = status.clone();
    let generation_live = generation.clone();
    start.connect_clicked(move |_| {
        let Some(id) = devices_start.active_id() else {
            status_live.set_text("Select a device and an ONVIF profile first");
            return;
        };
        let token = profiles_start.selected_row().and_then(|row| {
            profile_tokens_start
                .borrow()
                .get(row.index() as usize)
                .cloned()
        });
        let Some(token) = token else {
            status_live.set_text("Select an ONVIF profile first");
            return;
        };
        let device = repository_start
            .list()
            .ok()
            .and_then(|items| items.into_iter().find(|device| device.id.to_string() == id));
        let Some(device) = device else {
            status_live.set_text("The selected device no longer exists");
            return;
        };
        status_live.set_text("Requesting a live stream URI…");
        generation_live.set(generation_live.get().wrapping_add(1));
        let request_generation = generation_live.get();
        if let Some(mut old) = session_live.borrow_mut().take() {
            old.stop();
        }
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = send.send(live_stream_blocking(device, token));
        });
        let session = session_live.clone();
        let picture = picture_live.clone();
        let status = status_live.clone();
        let generation = generation_live.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            match receive.try_recv() {
                Ok(Ok((uri, credentials))) => {
                    if generation.get() != request_generation {
                        return gtk::glib::ControlFlow::Break;
                    }
                    match media::LiveSession::new(&picture).and_then(|mut value| {
                        value.start_rtsp(&uri, &credentials)?;
                        Ok(value)
                    }) {
                        Ok(value) => {
                            *session.borrow_mut() = Some(value);
                            status.set_text("RTSP pipeline started; waiting for decoded video…");
                        }
                        Err(error) => status.set_text(&format!("Live view setup failed: {error}")),
                    }
                }
                Ok(Err(error)) => status.set_text(&format!("Live stream request failed: {error}")),
                Err(mpsc::TryRecvError::Empty) => return gtk::glib::ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    status.set_text("Live stream worker stopped unexpectedly")
                }
            }
            gtk::glib::ControlFlow::Break
        });
    });
    let start_retry = start.clone();
    retry.connect_clicked(move |_| start_retry.emit_clicked());
    let session_stop = session.clone();
    let status_stop = status.clone();
    let generation_stop = generation.clone();
    stop.connect_clicked(move |_| {
        generation_stop.set(generation_stop.get().wrapping_add(1));
        if let Some(mut value) = session_stop.borrow_mut().take() {
            value.stop();
        }
        status_stop.set_text("Disconnected");
    });
    let session_events = session.clone();
    let status_events = status.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
        if let Some(session) = session_events.borrow_mut().as_mut() {
            let diagnostics = session.poll();
            match diagnostics.state {
                media::LiveState::Connecting => {
                    status_events.set_text("Connecting to RTSP stream…")
                }
                media::LiveState::Buffering => {
                    status_events.set_text("RTSP connected; buffering video…")
                }
                media::LiveState::ReceivingVideo => {
                    status_events.set_text("Receiving decoded video")
                }
                media::LiveState::Stalled => status_events
                    .set_text("No decoded video frames arrived before the startup timeout"),
                media::LiveState::Error => {
                    status_events.set_text("GStreamer stream, authentication, or decoder error")
                }
                media::LiveState::Disconnected => status_events.set_text("Disconnected"),
            }
        }
        gtk::glib::ControlFlow::Continue
    });
    view.append(&selectors);
    view.append(&picture);
    view.append(&controls);
    view.append(&row("Security", "The stream URI and credentials stay in process memory only and are never displayed or logged."));
    view
}

fn live_profiles_blocking(device: Device) -> Result<Vec<MediaProfile>, String> {
    let (client, _) = onvif_client(&device)?;
    let runtime = tokio::runtime::Runtime::new().map_err(|_| "async runtime unavailable")?;
    let report = runtime
        .block_on(client.inspect())
        .map_err(|error| error.to_string())?;
    if report.media_profiles.is_empty() {
        return Err("the device returned no ONVIF media profiles".into());
    }
    Ok(report.media_profiles)
}

fn live_stream_blocking(
    device: Device,
    profile_token: String,
) -> Result<(Url, Credentials), String> {
    let (client, credentials) = onvif_client(&device)?;
    let runtime = tokio::runtime::Runtime::new().map_err(|_| "async runtime unavailable")?;
    let uri = runtime
        .block_on(client.stream_uri(&profile_token))
        .map_err(|error| error.to_string())?;
    Ok((uri, credentials))
}
fn recordings_view() -> gtk::Box {
    let view = page(
        "Recordings",
        "Search, replay and export remain capability-gated.",
    );
    view.append(&row(
        "Replay unverified",
        "The authorized NVD currently returns RTSP DESCRIBE 503.",
    ));
    view
}
fn diagnostics_view() -> gtk::Box {
    let view = page(
        "Diagnostics",
        "Advertised services are not automatically considered operational.",
    );
    view.append(&row("Capability states", "Advertised, operation verified, playback verified, unsupported, authentication required, and unverified are distinct."));
    view
}
fn row(title: &str, detail: &str) -> gtk::ListBoxRow {
    let box_ = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .build();
    box_.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .build(),
    );
    box_.append(
        &gtk::Label::builder()
            .label(detail)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    gtk::ListBoxRow::builder().child(&box_).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_inspection_is_not_a_successful_connection() {
        let report = CompatibilityReport {
            device: None,
            services: Vec::new(),
            media_profiles: Vec::new(),
            capabilities: Vec::new(),
        };
        assert!(format_verified_inspection(report).is_err());
    }
}
