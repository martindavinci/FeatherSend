//! The main window, tray icon and prompts. Backend events arrive on a queue
//! and wake the GUI thread through an nwg `Notice`; yes/no decisions are
//! modal message boxes shown one at a time.

mod input;
mod settings;

use crate::win::{self, clipboard, hotkey, menu, system};
use feathersend_core::backend::{
    Command, DeviceClass, DeviceKind, DeviceView, IncomingRequest, PairOutcome, Reply, SendProblem,
    TransferState, TransferView, UiEvent,
};
use feathersend_core::clip::{ClipContent, EchoGuard, png_dimensions};
use feathersend_core::i18n::{Texts, fill, format_size};
use feathersend_core::settings::{DataDirs, Settings};
use feathersend_core::update::{Release, Version};
use native_windows_gui as nwg;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, MINMAXINFO, SetTimer};

pub enum AppEvent {
    Ui(UiEvent),
    Update(Release),
}

pub type Queue = Arc<Mutex<VecDeque<AppEvent>>>;

const HOTKEY_ID: i32 = 1;
const CLIP_TIMER: usize = 77;
const WM_GETMINMAXINFO: u32 = 0x0024;
const WM_HOTKEY: u32 = 0x0312;
const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const WM_TIMER: u32 = 0x0113;

enum Prompt {
    Incoming(IncomingRequest, Reply<bool>),
    Text { from: String, text: String },
    Clipboard { from: String, content: ClipContent, reply: Reply<bool> },
    Pair { alias: String, code: String, reply: Reply<bool> },
    Info { text: String, error: bool },
    Update(Release),
}

pub struct Context {
    pub queue: Queue,
    pub settings: Settings,
    pub dirs: DataDirs,
    pub texts: &'static Texts,
    pub exe: PathBuf,
    pub minimized: bool,
}

#[derive(Default)]
struct State {
    devices: Vec<DeviceView>,
    transfers: Vec<TransferView>,
    prompts: VecDeque<Prompt>,
    in_prompt: bool,
    /// Set while a batch of backend events is applied: prompts wait.
    deferring: bool,
    guard: EchoGuard,
    paused: bool,
    last_sequence: u32,
}

#[derive(Default)]
pub struct App {
    window: nwg::Window,
    icon: nwg::Icon,
    devices_label: nwg::Label,
    devices: nwg::ListView,
    send_files: nwg::Button,
    send_folder: nwg::Button,
    send_clipboard: nwg::Button,
    pair: nwg::Button,
    add_ip: nwg::Button,
    settings_button: nwg::Button,
    about: nwg::Button,
    transfers_label: nwg::Label,
    transfers: nwg::ListView,
    cancel: nwg::Button,
    resume: nwg::Button,
    tray: nwg::TrayNotification,
    notice: nwg::Notice,
    files_dialog: nwg::FileDialog,
    folder_dialog: nwg::FileDialog,
    settings_window: RefCell<Option<Rc<settings::SettingsWindow>>>,
    input_window: RefCell<Option<Rc<input::InputWindow>>>,
    backend: RefCell<Option<UnboundedSender<Command>>>,
    ctx: Option<Context>,
    state: RefCell<State>,
    settings: RefCell<Settings>,
}

fn column(list: &nwg::ListView, index: i32, text: &str, width: i32) {
    list.insert_column(nwg::InsertListViewColumn {
        index: Some(index),
        fmt: None,
        width: Some(width),
        text: Some(text.to_string()),
    });
}

fn cell(list: &nwg::ListView, row: i32, col: i32, text: String) {
    let item = nwg::InsertListViewItem { index: Some(row), column_index: col, text: Some(text) };
    if col == 0 { list.insert_item(item) } else { list.update_item(row as usize, item) }
}

impl App {
    fn t(&self) -> &'static Texts {
        self.ctx.as_ref().expect("context").texts
    }

    fn hwnd(&self) -> windows_sys::Win32::Foundation::HWND {
        win::hwnd_of(&self.window.handle)
    }

    pub fn build(ctx: Context) -> anyhow::Result<Rc<App>> {
        let t = ctx.texts;
        let mut app = App::default();
        *app.settings.get_mut() = ctx.settings.clone();
        let embed = nwg::EmbedResource::load(None)?;
        app.icon = embed.icon(1, None).ok_or_else(|| anyhow::anyhow!("icon resource missing"))?;
        nwg::Window::builder()
            .title(system::WINDOW_TITLE)
            .size((820, 560))
            .center(true)
            .icon(Some(&app.icon))
            .accept_files(true)
            .flags(
                nwg::WindowFlags::MAIN_WINDOW
                    | if ctx.minimized {
                        nwg::WindowFlags::empty()
                    } else {
                        nwg::WindowFlags::VISIBLE
                    },
            )
            .build(&mut app.window)?;
        let w = &app.window;
        nwg::Label::builder().parent(w).text(t.devices).build(&mut app.devices_label)?;
        nwg::Label::builder().parent(w).text(t.transfers).build(&mut app.transfers_label)?;
        for list in [&mut app.devices, &mut app.transfers] {
            nwg::ListView::builder()
                .parent(w)
                .list_style(nwg::ListViewStyle::Detailed)
                .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT | nwg::ListViewExFlags::GRID)
                .flags(
                    nwg::ListViewFlags::VISIBLE
                        | nwg::ListViewFlags::SINGLE_SELECTION
                        | nwg::ListViewFlags::TAB_STOP,
                )
                .build(list)?;
        }
        app.devices.set_headers_enabled(true);
        app.transfers.set_headers_enabled(true);
        column(&app.devices, 0, t.col_name, 200);
        column(&app.devices, 1, t.col_address, 150);
        column(&app.devices, 2, t.col_type, 90);
        column(&app.devices, 3, t.col_status, 160);
        column(&app.transfers, 0, t.col_device, 140);
        column(&app.transfers, 1, t.col_item, 220);
        column(&app.transfers, 2, t.col_progress, 150);
        column(&app.transfers, 3, t.col_state, 120);
        for (button, text) in [
            (&mut app.send_files, t.send_files),
            (&mut app.send_folder, t.send_folder),
            (&mut app.send_clipboard, t.send_clipboard),
            (&mut app.pair, t.pair),
            (&mut app.add_ip, t.add_by_ip),
            (&mut app.settings_button, t.settings),
            (&mut app.about, t.about),
            (&mut app.cancel, t.cancel),
            (&mut app.resume, t.resume),
        ] {
            nwg::Button::builder().parent(w).text(text).build(button)?;
        }
        nwg::TrayNotification::builder()
            .parent(w)
            .icon(Some(&app.icon))
            .tip(Some("FeatherSend"))
            .build(&mut app.tray)?;
        nwg::Notice::builder().parent(w).build(&mut app.notice)?;
        nwg::FileDialog::builder()
            .action(nwg::FileDialogAction::Open)
            .multiselect(true)
            .title(t.send_files)
            .build(&mut app.files_dialog)?;
        nwg::FileDialog::builder()
            .action(nwg::FileDialogAction::OpenDirectory)
            .title(t.send_folder)
            .build(&mut app.folder_dialog)?;
        app.ctx = Some(ctx);
        let app = Rc::new(app);
        app.layout();
        app.bind();
        app.register_hotkey();
        clipboard::add_listener(app.hwnd());
        app.state.borrow_mut().last_sequence = clipboard::sequence_number();
        Ok(app)
    }

    pub fn window_handle(&self) -> nwg::ControlHandle {
        self.window.handle
    }

    pub fn notice_sender(&self) -> nwg::NoticeSender {
        self.notice.sender()
    }

    pub fn set_backend(&self, sender: UnboundedSender<Command>) {
        *self.backend.borrow_mut() = Some(sender);
    }

    fn send(&self, command: Command) {
        if let Some(backend) = self.backend.borrow().as_ref() {
            let _ = backend.send(command);
        }
    }

    fn bind(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        nwg::full_bind_event_handler(&self.window.handle, move |event, data, handle| {
            if let Some(app) = weak.upgrade() {
                app.on_event(event, data, handle);
            }
        });
        let weak = Rc::downgrade(self);
        let _ = nwg::bind_raw_event_handler(
            &self.window.handle,
            0x10_000,
            move |_hwnd, msg, wparam, lparam| {
                let app = weak.upgrade()?;
                match msg {
                    WM_GETMINMAXINFO => {
                        let info = unsafe { &mut *(lparam as *mut MINMAXINFO) };
                        info.ptMinTrackSize.x = 720;
                        info.ptMinTrackSize.y = 480;
                        Some(0)
                    }
                    WM_HOTKEY if wparam as i32 == HOTKEY_ID => {
                        app.send_clipboard_action(false);
                        Some(0)
                    }
                    WM_CLIPBOARDUPDATE => {
                        app.clipboard_changed();
                        Some(0)
                    }
                    WM_TIMER if wparam == CLIP_TIMER => {
                        unsafe { KillTimer(app.hwnd(), CLIP_TIMER) };
                        app.sync_clipboard();
                        Some(0)
                    }
                    _ => None,
                }
            },
        );
    }

    fn register_hotkey(&self) {
        let hwnd = self.hwnd();
        hotkey::unregister(hwnd, HOTKEY_ID);
        let key = self.settings.borrow().clipboard_hotkey.clone();
        if !hotkey::register(hwnd, HOTKEY_ID, &key) {
            tracing::warn!("cannot register the hotkey {key}");
        }
    }

    fn layout(&self) {
        let (w, h) = self.window.size();
        let (w, h) = (w as i32, h as i32);
        let m = 10;
        let side = 170;
        let list_w = (w - side - 3 * m).max(200);
        let top = m + 22;
        let devices_h = ((h - 2 * top - 2 * m) * 55 / 100).max(150);
        self.devices_label.set_position(m, m);
        self.devices_label.set_size(list_w as u32, 20);
        self.devices.set_position(m, top);
        self.devices.set_size(list_w as u32, devices_h as u32);
        let bx = m + list_w + m;
        for (i, button) in [
            &self.send_files,
            &self.send_folder,
            &self.send_clipboard,
            &self.pair,
            &self.add_ip,
            &self.settings_button,
            &self.about,
        ]
        .into_iter()
        .enumerate()
        {
            button.set_position(bx, top + i as i32 * 34);
            button.set_size(side as u32, 30);
        }
        let label_y = top + devices_h + m;
        self.transfers_label.set_position(m, label_y);
        self.transfers_label.set_size(list_w as u32, 20);
        let list_y = label_y + 22;
        self.transfers.set_position(m, list_y);
        self.transfers.set_size(list_w as u32, (h - list_y - m).max(80) as u32);
        self.cancel.set_position(bx, list_y);
        self.cancel.set_size(side as u32, 30);
        self.resume.set_position(bx, list_y + 34);
        self.resume.set_size(side as u32, 30);
    }

    fn on_event(
        self: &Rc<Self>,
        event: nwg::Event,
        data: nwg::EventData,
        handle: nwg::ControlHandle,
    ) {
        use nwg::Event as E;
        match event {
            E::OnWindowClose if handle == self.window.handle => {
                if let nwg::EventData::OnWindowClose(close) = data {
                    close.close(false);
                }
                self.window.set_visible(false);
            }
            E::OnResize if handle == self.window.handle => self.layout(),
            E::OnNotice if handle == self.notice.handle => self.drain(),
            E::OnFileDrop if handle == self.window.handle => {
                if let nwg::EventData::OnFileDrop(drop) = data {
                    let paths: Vec<PathBuf> = drop.files().into_iter().map(PathBuf::from).collect();
                    if let Some(device) = self.selected_device_or_tell() {
                        self.send(Command::SendFiles { device: device.fingerprint, paths });
                    }
                }
            }
            E::OnButtonClick => self.on_button(handle),
            E::OnListViewRightClick if handle == self.devices.handle => self.device_menu(),
            E::OnListViewDoubleClick if handle == self.devices.handle => self.send_files_action(),
            E::OnContextMenu if handle == self.tray.handle => self.tray_menu(),
            E::OnMousePress(nwg::MousePressEvent::MousePressLeftUp)
                if handle == self.tray.handle =>
            {
                self.show_window()
            }
            _ => {}
        }
    }

    fn on_button(self: &Rc<Self>, handle: nwg::ControlHandle) {
        if handle == self.send_files.handle {
            self.send_files_action();
        } else if handle == self.send_folder.handle {
            self.send_folder_action();
        } else if handle == self.send_clipboard.handle {
            self.send_clipboard_action(true);
        } else if handle == self.pair.handle {
            if let Some(device) = self.selected_device_or_tell() {
                self.send(Command::Pair { device: device.fingerprint });
            }
        } else if handle == self.add_ip.handle {
            self.add_ip_action();
        } else if handle == self.settings_button.handle {
            self.settings_action();
        } else if handle == self.about.handle {
            let text = fill(self.t().about_body, &[("version", &Version::current().to_string())]);
            self.queue_prompt(Prompt::Info { text, error: false });
        } else if handle == self.cancel.handle {
            if let Some(id) = self.selected_transfer() {
                self.send(Command::CancelTransfer { id });
            }
        } else if handle == self.resume.handle
            && let Some(id) = self.selected_transfer()
        {
            self.send(Command::ResumeTransfer { id });
        }
    }

    pub fn show_window(&self) {
        self.window.set_visible(true);
        self.window.restore();
        self.window.set_focus();
    }

    fn balloon(&self, text: &str) {
        self.tray.show(
            text,
            Some("FeatherSend"),
            Some(nwg::TrayNotificationFlags::USER_ICON),
            Some(&self.icon),
        );
    }

    // ----- devices -------------------------------------------------------

    fn selected_device(&self) -> Option<DeviceView> {
        let index = self.devices.selected_item()?;
        self.state.borrow().devices.get(index).cloned()
    }

    fn selected_device_or_tell(self: &Rc<Self>) -> Option<DeviceView> {
        let device = self.selected_device();
        if device.is_none() {
            self.queue_prompt(Prompt::Info { text: self.t().select_device.into(), error: false });
        }
        device
    }

    fn refresh_devices(&self, selected: Option<String>) {
        let t = self.t();
        let state = self.state.borrow();
        self.devices.set_redraw(false);
        self.devices.clear();
        for (row, d) in state.devices.iter().enumerate() {
            let row = row as i32;
            let kind = match d.kind {
                DeviceKind::Paired => t.status_paired,
                DeviceKind::FeatherSend => t.status_feathersend,
                DeviceKind::LocalSend => t.status_localsend,
            };
            let status =
                if d.online { kind.to_string() } else { format!("{kind} ({})", t.status_offline) };
            let class = match d.class {
                DeviceClass::Mobile => t.type_mobile,
                DeviceClass::Desktop => t.type_desktop,
                DeviceClass::Other => t.type_other,
            };
            cell(&self.devices, row, 0, d.alias.clone());
            cell(&self.devices, row, 1, d.address.clone().unwrap_or_default());
            cell(&self.devices, row, 2, class.to_string());
            cell(&self.devices, row, 3, status);
        }
        if let Some(fp) = selected
            && let Some(i) = state.devices.iter().position(|d| d.fingerprint == fp)
        {
            self.devices.select_item(i, true);
        }
        self.devices.set_redraw(true);
    }

    fn device_menu(self: &Rc<Self>) {
        let t = self.t();
        let Some(d) = self.selected_device() else { return };
        let paired = d.kind == DeviceKind::Paired;
        use menu::Item;
        let chosen = menu::popup(
            self.hwnd(),
            &[
                Item::new(1, t.send_files),
                Item::new(2, t.send_folder),
                Item::new(3, t.send_clipboard),
                Item::Separator,
                Item::check(4, t.pair, false, !paired && d.online),
                Item::check(5, t.unpair, false, paired),
                Item::Separator,
                Item::check(6, t.menu_auto_accept, d.auto_accept, paired),
                Item::check(7, t.menu_auto_sync, d.auto_sync, paired),
            ],
        );
        match chosen {
            1 => self.send_files_action(),
            2 => self.send_folder_action(),
            3 => self.send_clipboard_action(true),
            4 => self.send(Command::Pair { device: d.fingerprint }),
            5 => {
                let text = fill(t.unpair_confirm, &[("device", &d.alias)]);
                if self.yes_no(t.unpair, &text) {
                    self.send(Command::Unpair { fingerprint: d.fingerprint });
                }
            }
            6 => self.send(Command::SetDeviceFlags {
                fingerprint: d.fingerprint,
                auto_accept: Some(!d.auto_accept),
                auto_sync: None,
            }),
            7 => self.send(Command::SetDeviceFlags {
                fingerprint: d.fingerprint,
                auto_accept: None,
                auto_sync: Some(!d.auto_sync),
            }),
            _ => {}
        }
    }

    fn tray_menu(self: &Rc<Self>) {
        let t = self.t();
        let paused = self.state.borrow().paused;
        use menu::Item;
        let chosen = menu::popup(
            self.hwnd(),
            &[
                Item::new(1, t.tray_open),
                Item::new(2, t.send_clipboard),
                Item::check(3, t.tray_pause, paused, true),
                Item::Separator,
                Item::new(4, t.tray_quit),
            ],
        );
        match chosen {
            1 => self.show_window(),
            2 => self.send_clipboard_action(false),
            3 => {
                self.state.borrow_mut().paused = !paused;
                self.send(Command::SetPaused(!paused));
            }
            4 => {
                self.tray.set_visibility(false);
                nwg::stop_thread_dispatch();
            }
            _ => {}
        }
    }

    // ----- actions -------------------------------------------------------

    fn send_files_action(self: &Rc<Self>) {
        let Some(device) = self.selected_device_or_tell() else { return };
        if self.files_dialog.run(Some(&self.window))
            && let Ok(items) = self.files_dialog.get_selected_items()
        {
            let paths = items.into_iter().map(PathBuf::from).collect();
            self.send(Command::SendFiles { device: device.fingerprint, paths });
        }
    }

    fn send_folder_action(self: &Rc<Self>) {
        let Some(device) = self.selected_device_or_tell() else { return };
        if self.folder_dialog.run(Some(&self.window))
            && let Ok(item) = self.folder_dialog.get_selected_item()
        {
            self.send(Command::SendFiles { device: device.fingerprint, paths: vec![item.into()] });
        }
    }

    /// The button sends to the selected device. The hotkey and the tray
    /// (spec §12) send directly only when exactly one device qualifies,
    /// preferring paired ones; otherwise they show a chooser.
    fn send_clipboard_action(self: &Rc<Self>, use_selection: bool) {
        let t = self.t();
        let Some(content) = clipboard::read(self.hwnd()) else {
            self.balloon(t.clipboard_empty);
            return;
        };
        let target = if use_selection {
            match self.selected_device_or_tell() {
                Some(d) => Some(d),
                None => return,
            }
        } else {
            let mut online: Vec<DeviceView> =
                self.state.borrow().devices.iter().filter(|d| d.online).cloned().collect();
            // Paired devices first in the chooser.
            online.sort_by_key(|d| d.kind != DeviceKind::Paired);
            if online.is_empty() {
                self.balloon(t.no_devices_online);
                None
            } else if online.len() == 1 {
                Some(online[0].clone())
            } else {
                let items: Vec<menu::Item> = online
                    .iter()
                    .enumerate()
                    .map(|(i, d)| menu::Item::new(100 + i as u32, &d.alias))
                    .collect();
                let chosen = menu::popup(self.hwnd(), &items);
                chosen.checked_sub(100).and_then(|i| online.get(i as usize).cloned())
            }
        };
        if let Some(device) = target {
            self.send(Command::SendClipboard { device: device.fingerprint, content });
        }
    }

    fn clipboard_changed(&self) {
        let sequence = clipboard::sequence_number();
        let syncing = {
            let mut state = self.state.borrow_mut();
            let changed = sequence != state.last_sequence;
            state.last_sequence = sequence;
            changed && state.devices.iter().any(|d| d.online && d.auto_sync)
        };
        if syncing {
            unsafe { SetTimer(self.hwnd(), CLIP_TIMER, 300, None) };
        }
    }

    fn sync_clipboard(&self) {
        let Some(content) = clipboard::read(self.hwnd()) else { return };
        let Some(bytes) = content.fingerprint_bytes() else { return };
        if self.state.borrow_mut().guard.should_skip(bytes) {
            return;
        }
        self.send(Command::SyncClipboard { content });
    }

    fn write_clipboard(&self, content: &ClipContent) {
        if let Err(e) = clipboard::write(self.hwnd(), content) {
            tracing::warn!("cannot write the clipboard: {e:#}");
            return;
        }
        // Remember what a later read returns (text comes back with CRLF line
        // ends), so the change notification is not synced back.
        let written = clipboard::read(self.hwnd());
        if let Some(bytes) = written.as_ref().and_then(|c| c.fingerprint_bytes()) {
            self.state.borrow_mut().guard.note_written(bytes);
        }
    }

    fn add_ip_action(self: &Rc<Self>) {
        let t = self.t();
        let window = self
            .input_window
            .borrow_mut()
            .get_or_insert_with(|| {
                input::InputWindow::build(&self.window, &self.icon, t.ok, t.cancel)
            })
            .clone();
        let weak = Rc::downgrade(self);
        window.ask(
            t.add_ip_prompt,
            Box::new(move |text| {
                let Some(app) = weak.upgrade() else { return };
                let text = text.trim();
                let (ip, port) = match text.rsplit_once(':') {
                    Some((ip, port)) if !ip.contains(':') || ip.ends_with(']') => {
                        (ip.trim_matches(['[', ']']), port.parse().ok())
                    }
                    _ => (text, None),
                };
                match ip.parse() {
                    Ok(ip) => app.send(Command::AddByIp { ip, port }),
                    Err(_) => app.queue_prompt(Prompt::Info {
                        text: app.t().invalid_ip.into(),
                        error: true,
                    }),
                }
            }),
        );
    }

    fn settings_action(self: &Rc<Self>) {
        let t = self.t();
        let window = self
            .settings_window
            .borrow_mut()
            .get_or_insert_with(|| settings::SettingsWindow::build(&self.window, &self.icon, t))
            .clone();
        let weak = Rc::downgrade(self);
        let current = self.settings.borrow().clone();
        window.open(
            &current,
            system::autostart_enabled(),
            Box::new(move |new| {
                if let Some(app) = weak.upgrade() {
                    app.apply_settings(new);
                }
            }),
        );
    }

    fn apply_settings(self: &Rc<Self>, new: Settings) {
        let ctx = self.ctx.as_ref().expect("context");
        let old = self.settings.replace(new.clone());
        if let Err(e) = new.save(&ctx.dirs.settings_file) {
            tracing::warn!("cannot save settings: {e}");
        }
        if let Err(e) = system::set_autostart(new.start_with_windows, &ctx.exe) {
            tracing::warn!("cannot change autostart: {e}");
        }
        self.register_hotkey();
        self.send(Command::ApplySettings(new.clone()));
        if old.alias != new.alias
            || old.language != new.language
            || old.localsend_port != new.localsend_port
        {
            self.queue_prompt(Prompt::Info { text: self.t().restart_needed.into(), error: false });
        }
    }

    // ----- transfers -----------------------------------------------------

    fn selected_transfer(&self) -> Option<u64> {
        let index = self.transfers.selected_item()?;
        self.state.borrow().transfers.get(index).map(|t| t.id)
    }

    fn refresh_transfers(&self, selected: Option<u64>) {
        let t = self.t();
        let state = self.state.borrow();
        self.transfers.set_redraw(false);
        self.transfers.clear();
        for (row, v) in state.transfers.iter().enumerate() {
            let row = row as i32;
            let percent = (v.done * 100).checked_div(v.total).unwrap_or(100);
            let state_text = match &v.state {
                TransferState::Waiting => t.state_waiting.to_string(),
                TransferState::Running => t.state_running.to_string(),
                TransferState::Done => t.state_done.to_string(),
                TransferState::Failed(reason) => fill(t.state_failed, &[("reason", reason)]),
                TransferState::Cancelled => t.state_cancelled.to_string(),
                TransferState::Paused => t.state_paused.to_string(),
            };
            cell(&self.transfers, row, 0, v.device.clone());
            cell(
                &self.transfers,
                row,
                1,
                format!("{} {}", if v.incoming { "↓" } else { "↑" }, v.item),
            );
            cell(
                &self.transfers,
                row,
                2,
                format!("{percent}% · {} / {}", format_size(v.done), format_size(v.total)),
            );
            cell(&self.transfers, row, 3, state_text);
        }
        if let Some(id) = selected
            && let Some(i) = state.transfers.iter().position(|v| v.id == id)
        {
            self.transfers.select_item(i, true);
        }
        self.transfers.set_redraw(true);
    }

    // ----- backend events ------------------------------------------------

    /// Applies every queued event first, then shows the prompts they raised:
    /// a modal box pumps messages, so showing one mid-batch would let a
    /// nested drain apply newer events before older ones.
    fn drain(self: &Rc<Self>) {
        let events: Vec<AppEvent> = {
            let queue = &self.ctx.as_ref().expect("context").queue;
            queue.lock().unwrap().drain(..).collect()
        };
        // Selections are kept by identity, not by row: rows move.
        let selected_device = self.selected_device().map(|d| d.fingerprint);
        let selected_transfer = self.selected_transfer();
        self.state.borrow_mut().deferring = true;
        let (mut devices_changed, mut transfers_changed) = (false, false);
        for event in events {
            match event {
                AppEvent::Update(release) => self.queue_prompt(Prompt::Update(release)),
                AppEvent::Ui(event) => match event {
                    UiEvent::Devices(list) => {
                        self.state.borrow_mut().devices = list;
                        devices_changed = true;
                    }
                    UiEvent::Transfer(view) => {
                        let mut state = self.state.borrow_mut();
                        match state.transfers.iter_mut().find(|v| v.id == view.id) {
                            Some(existing) => *existing = view,
                            None => state.transfers.insert(0, view),
                        }
                        state.transfers.truncate(200);
                        transfers_changed = true;
                    }
                    UiEvent::IncomingTransfer { request, reply } => {
                        self.queue_prompt(Prompt::Incoming(request, reply))
                    }
                    UiEvent::IncomingText { from, text } => {
                        self.queue_prompt(Prompt::Text { from, text })
                    }
                    UiEvent::IncomingClipboard { from, content, reply } => {
                        self.queue_prompt(Prompt::Clipboard { from, content, reply })
                    }
                    UiEvent::ClipboardReceived { from, content } => {
                        self.write_clipboard(&content);
                        self.balloon(&fill(self.t().clipboard_received, &[("device", &from)]));
                    }
                    UiEvent::PairCode { alias, code, reply } => {
                        self.queue_prompt(Prompt::Pair { alias, code, reply })
                    }
                    UiEvent::PairResult { alias, outcome } => {
                        let t = self.t();
                        let (text, error) = match outcome {
                            PairOutcome::Paired => (t.pair_ok, false),
                            PairOutcome::Failed => (t.pair_failed, true),
                            PairOutcome::NotFeatherSend => (t.pair_needs_feathersend, true),
                        };
                        let text = fill(text, &[("device", &alias)]);
                        self.queue_prompt(Prompt::Info { text, error });
                    }
                    UiEvent::Received { from, item } => {
                        let text =
                            fill(self.t().received_notice, &[("item", &item), ("device", &from)]);
                        self.balloon(&text);
                    }
                    UiEvent::SendFailed { device, problem, detail } => {
                        let t = self.t();
                        let text = match problem {
                            SendProblem::Declined => fill(t.declined, &[("device", &device)]),
                            SendProblem::Busy => fill(t.busy, &[("device", &device)]),
                            SendProblem::Offline => {
                                format!("{}{device} ({})", t.error_prefix, t.status_offline)
                            }
                            SendProblem::Other => format!("{}{device}: {detail}", t.error_prefix),
                        };
                        self.queue_prompt(Prompt::Info {
                            text,
                            error: problem != SendProblem::Declined,
                        });
                    }
                },
            }
        }
        if devices_changed {
            self.refresh_devices(selected_device);
        }
        if transfers_changed {
            self.refresh_transfers(selected_transfer);
        }
        self.state.borrow_mut().deferring = false;
        self.pump_prompts();
    }

    // ----- prompts -------------------------------------------------------

    fn queue_prompt(self: &Rc<Self>, prompt: Prompt) {
        let deferring = {
            let mut state = self.state.borrow_mut();
            state.prompts.push_back(prompt);
            state.deferring
        };
        if !deferring {
            self.pump_prompts();
        }
    }

    /// Shows queued prompts one at a time. A modal box keeps pumping
    /// messages, so new events only queue while one is open.
    fn pump_prompts(self: &Rc<Self>) {
        loop {
            let prompt = {
                let mut state = self.state.borrow_mut();
                if state.in_prompt {
                    return;
                }
                let Some(prompt) = state.prompts.pop_front() else { return };
                state.in_prompt = true;
                prompt
            };
            self.show_prompt(prompt);
            self.state.borrow_mut().in_prompt = false;
        }
    }

    fn yes_no(&self, title: &str, text: &str) -> bool {
        let params = nwg::MessageParams {
            title,
            content: text,
            buttons: nwg::MessageButtons::YesNo,
            icons: nwg::MessageIcons::Question,
        };
        nwg::modal_message(&self.window, &params) == nwg::MessageChoice::Yes
    }

    fn show_prompt(&self, prompt: Prompt) {
        let t = self.t();
        match prompt {
            Prompt::Incoming(request, reply) => {
                let mut names = request.names.join("\n");
                if request.count > request.names.len() {
                    let more = (request.count - request.names.len()).to_string();
                    names.push('\n');
                    names.push_str(&fill(t.incoming_more, &[("n", &more)]));
                }
                let text = fill(
                    t.incoming_body,
                    &[
                        ("device", &request.from),
                        ("count", &request.count.to_string()),
                        ("size", &format_size(request.total_bytes)),
                        ("names", &names),
                    ],
                );
                let _ = reply.send(self.yes_no(t.incoming_title, &text));
            }
            Prompt::Text { from, text } => {
                let shown: String = text.chars().take(600).collect();
                let body = fill(t.message_body, &[("device", &from), ("text", &shown)]);
                if self.yes_no(t.message_title, &body) {
                    self.write_clipboard(&ClipContent::Text(text));
                }
            }
            Prompt::Clipboard { from, content, reply } => {
                let body = match &content {
                    ClipContent::Text(text) => fill(
                        t.clipboard_text_body,
                        &[("device", &from), ("chars", &text.chars().count().to_string())],
                    ),
                    ClipContent::Png(png) => {
                        let (w, h) = png_dimensions(png).unwrap_or_default();
                        fill(
                            t.clipboard_image_body,
                            &[("device", &from), ("w", &w.to_string()), ("h", &h.to_string())],
                        )
                    }
                    ClipContent::Files(_) => fill(t.clipboard_received, &[("device", &from)]),
                };
                let _ = reply.send(self.yes_no(t.clipboard_title, &body));
            }
            Prompt::Pair { alias, code, reply } => {
                let body = fill(t.pair_body, &[("device", &alias), ("code", &code)]);
                self.show_window();
                let _ = reply.send(self.yes_no(t.pair_title, &body));
            }
            Prompt::Info { text, error } => {
                if error {
                    nwg::modal_error_message(&self.window, "FeatherSend", &text);
                } else {
                    nwg::modal_info_message(&self.window, "FeatherSend", &text);
                }
            }
            Prompt::Update(release) => {
                let body = fill(t.update_available, &[("version", &release.version.to_string())]);
                if self.yes_no("FeatherSend", &body) {
                    system::open_url(&release.url);
                }
            }
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let hwnd = win::hwnd_of(&self.window.handle);
        if !hwnd.is_null() {
            hotkey::unregister(hwnd, HOTKEY_ID);
            clipboard::remove_listener(hwnd);
        }
    }
}
