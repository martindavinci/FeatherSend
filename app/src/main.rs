#![windows_subsystem = "windows"]

//! FeatherSend: share files, folders and the clipboard between devices on
//! the local network. Runs on Windows 7 SP1 and later.

mod gui;
mod logging;
mod win;

use feathersend_core::backend::{Backend, BackendConfig, Command, EventSink, NetworkHint};
use feathersend_core::i18n::{Language, fill};
use feathersend_core::identity::Identity;
use feathersend_core::settings::{DataDirs, Settings};
use feathersend_core::update;
use gui::{App, AppEvent, Context, Queue};
use native_windows_gui as nwg;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use win::system;

fn main() {
    let minimized = std::env::args().any(|a| a == "--minimized");
    let Some(_instance) = system::acquire_instance() else {
        system::show_existing();
        return;
    };
    if let Err(e) = run(minimized) {
        tracing::error!("fatal: {e:#}");
        nwg::error_message("FeatherSend", &format!("{e:#}"));
    }
}

fn run(minimized: bool) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let appdata = std::env::var_os("APPDATA").map(std::path::PathBuf::from);
    let dirs = DataDirs::resolve(&exe_dir, appdata.as_deref());
    dirs.ensure()?;
    logging::init(&dirs.logs_dir);
    tracing::info!(
        "FeatherSend {} starting, data in {}",
        update::Version::current(),
        dirs.root.display()
    );

    let defaults =
        Settings::defaults(system::computer_name(), system::downloads().join("FeatherSend"));
    let settings = Settings::load(&dirs.settings_file, &defaults);
    if !dirs.settings_file.exists() {
        let _ = settings.save(&dirs.settings_file);
    }
    let texts = Language::resolve(&settings.language, &system::ui_languages()).texts();
    logging::set_crash_text(texts.crash_message);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("feathersend-net")
        .build()?;
    // First run generates the RSA identity: a few seconds on old CPUs.
    let identity_file = dirs.identity_file.clone();
    let identity = runtime.block_on(async {
        tokio::task::spawn_blocking(move || Identity::load_or_generate(&identity_file)).await?
    })?;

    nwg::init()?;
    let mut font = nwg::Font::default();
    if nwg::Font::builder().family("Segoe UI").size(16).build(&mut font).is_ok() {
        let _ = nwg::Font::set_global_default(Some(font));
    }

    let queue: Queue = Arc::new(Mutex::new(Default::default()));
    let app = App::build(Context {
        queue: queue.clone(),
        settings: settings.clone(),
        dirs: dirs.clone(),
        texts,
        exe: exe.clone(),
        minimized,
    })?;

    let notice = app.notice_sender();
    let sink_queue = queue.clone();
    let sink: EventSink = Arc::new(move |event| {
        sink_queue.lock().unwrap().push_back(AppEvent::Ui(event));
        notice.notice();
    });
    let port = settings.localsend_port;
    let backend = match runtime
        .block_on(Backend::start(BackendConfig::new(dirs, settings.clone(), identity), sink))
    {
        Ok(backend) => backend,
        Err(e) => {
            tracing::error!("backend failed to start: {e:#}");
            nwg::modal_error_message(
                app_window_handle(&app),
                "FeatherSend",
                &fill(texts.port_in_use, &[("port", &port.to_string())]),
            );
            return Ok(());
        }
    };
    app.set_backend(backend.sender());
    let hints = backend.sender();
    win::netwatch::spawn(move || {
        let _ = hints.send(Command::NetworkChanged(NetworkHint::AddressChange));
    });

    if settings.check_updates {
        let update_queue = queue.clone();
        runtime.spawn(async move {
            loop {
                match update::check_latest(update::REPO).await {
                    Ok(Some(release)) => {
                        update_queue.lock().unwrap().push_back(AppEvent::Update(release));
                        notice.notice();
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::info!("update check failed: {e:#}"),
                }
                tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
            }
        });
    }

    // Manual check of the panic hook (docs/manual-test.md).
    if std::env::var_os("FEATHERSEND_PANIC_TEST").is_some() {
        let _ = std::thread::Builder::new()
            .name("panic-test".into())
            .spawn(|| panic!("FEATHERSEND_PANIC_TEST is set"));
    }

    nwg::dispatch_thread_events();
    tracing::info!("shutting down");
    drop(app);
    runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(3), backend.shutdown()).await })
        .ok();
    runtime.shutdown_timeout(Duration::from_secs(1));
    Ok(())
}

fn app_window_handle(app: &App) -> nwg::ControlHandle {
    app.window_handle()
}
