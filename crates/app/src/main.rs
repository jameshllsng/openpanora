use gtk::prelude::*;
use openpanora_core::{DeviceRepository, SqliteDeviceRepository};
use std::{path::PathBuf, rc::Rc};

const APP_ID: &str = "org.openpanora.OpenPanora";

fn main() -> gtk::glib::ExitCode {
    let application = gtk::Application::builder().application_id(APP_ID).build();
    application.connect_activate(build_ui);
    application.run()
}

fn build_ui(application: &gtk::Application) {
    let repository = open_repository().map(Rc::new);
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .title("OpenPanora")
        .default_width(1180)
        .default_height(760)
        .build();

    let header = gtk::HeaderBar::new();
    let title = gtk::Label::builder()
        .label("OpenPanora")
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title));
    window.set_titlebar(Some(&header));

    let stack = gtk::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    stack.add_titled(
        &devices_view(repository.as_deref()),
        Some("devices"),
        "Devices",
    );
    stack.add_titled(&live_view(), Some("live"), "Live View");
    stack.add_titled(&recordings_view(), Some("recordings"), "Recordings");
    stack.add_titled(
        &diagnostics_view(repository.as_ref().err()),
        Some("diagnostics"),
        "Diagnostics",
    );

    let sidebar = gtk::StackSidebar::builder()
        .stack(&stack)
        .width_request(210)
        .build();
    sidebar.add_css_class("navigation-sidebar");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content.append(&sidebar);
    content.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    content.append(&stack);

    window.set_child(Some(&content));
    window.present();
}

fn open_repository() -> Result<SqliteDeviceRepository, String> {
    let mut directory = gtk::glib::user_data_dir();
    directory.push("openpanora");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path: PathBuf = directory.join("openpanora.sqlite3");
    SqliteDeviceRepository::open(path).map_err(|error| error.to_string())
}

fn page(title: &str, subtitle: &str) -> gtk::Box {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(28)
        .margin_bottom(28)
        .margin_start(32)
        .margin_end(32)
        .build();
    container.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["title-1"])
            .build(),
    );
    container.append(
        &gtk::Label::builder()
            .label(subtitle)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    container
}

fn devices_view(repository: Result<&SqliteDeviceRepository, &String>) -> gtk::Box {
    let container = page(
        "Devices",
        "Registered cameras, DVRs, and NVRs are stored locally in SQLite.",
    );
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    match repository {
        Ok(repository) => match repository.list() {
            Ok(devices) if devices.is_empty() => list.append(&status_row(
                "No devices registered",
                "Use device-probe to verify compatibility before registration UI is enabled.",
            )),
            Ok(devices) => {
                for device in devices {
                    list.append(&status_row(
                        &device.name,
                        &format!("{}:{} · {:?}", device.host, device.port, device.kind),
                    ));
                }
            }
            Err(error) => list.append(&status_row("Database error", &error.to_string())),
        },
        Err(error) => list.append(&status_row("Database unavailable", error)),
    }
    container.append(&list);
    container
}

fn live_view() -> gtk::Box {
    let container = page(
        "Live View",
        "The native workspace is ready for GStreamer-backed sessions and 1/4/9/16/36 layouts.",
    );
    container.append(&status_row(
        "No active streams",
        "Stream controls remain disabled until a verified device and media profile are selected.",
    ));
    container
}

fn recordings_view() -> gtk::Box {
    let container = page(
        "Recordings",
        "Remote search, replay, timeline navigation, and time-range export are first-class application contracts.",
    );
    container.append(&status_row(
        "Device capability required",
        "Use device-probe capabilities to verify Recording Search and Replay services for each model and firmware.",
    ));
    container
}

fn diagnostics_view(repository_error: Option<&String>) -> gtk::Box {
    let container = page(
        "Diagnostics",
        "Local environment status. No network discovery runs automatically.",
    );
    let database_status = repository_error
        .map(|error| format!("Unavailable: {error}"))
        .unwrap_or_else(|| "Ready".into());
    container.append(&status_row("SQLite device catalog", &database_status));
    container.append(&status_row(
        "Compatibility probe",
        "Run `cargo run -p device-probe -- --help` in a terminal.",
    ));
    container
}

fn status_row(title: &str, detail: &str) -> gtk::ListBoxRow {
    let labels = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(14)
        .margin_end(14)
        .build();
    labels.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .build(),
    );
    labels.append(
        &gtk::Label::builder()
            .label(detail)
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build(),
    );
    gtk::ListBoxRow::builder().child(&labels).build()
}
