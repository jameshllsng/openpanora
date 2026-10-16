#![allow(deprecated)]

use device_probe::{
    auth::Credentials,
    onvif::{HttpSoapTransport, OnvifClient},
};
use gtk::prelude::*;
use openpanora_core::{Device, DeviceRepository, SqliteDeviceRepository};
use std::{rc::Rc, sync::mpsc, time::Duration};
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
    stack.add_titled(&live_view(), Some("live"), "Live View");
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
    if let Some(device) = &existing {
        name.set_text(&device.name);
        endpoint.set_text(&device.host);
        username.set_text(device.credential_key.as_deref().unwrap_or(""));
    }
    for (index, (label, field)) in [
        ("Display name", name.clone().upcast::<gtk::Widget>()),
        ("ONVIF service URL", endpoint.clone().upcast()),
        ("Username", username.clone().upcast()),
        ("Password (Secret Service)", password.clone().upcast()),
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
    let runtime = tokio::runtime::Runtime::new().map_err(|_| "async runtime unavailable")?;
    let transport = HttpSoapTransport::new(
        Duration::from_secs(10),
        Some(Credentials { username, password }),
        false,
        None,
    )
    .map_err(|error| error.to_string())?;
    let report = runtime
        .block_on(OnvifClient::new(transport, endpoint, None, None).inspect())
        .map_err(|error| error.to_string())?;
    Ok(format!(
        "Connected: {} service(s), {} media profile(s)",
        report.services.len(),
        report.media_profiles.len()
    ))
}

fn live_view() -> gtk::Box {
    let view = page(
        "Live View",
        "Choose a discovered channel to prepare a native GStreamer video surface.",
    );
    view.append(&row(
        "No channel selected",
        "Live playback is not yet verified in the GTK interface.",
    ));
    view
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
