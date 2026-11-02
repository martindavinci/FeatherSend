//! The main window, tray icon and prompts. Backend events arrive on a queue
//! and wake the GUI thread through an nwg `Notice`; yes/no decisions are
//! modal message boxes shown one at a time.

mod icons;
mod input;
mod layout;
mod rows;
mod settings;
mod view;

use crate::win::{self, clipboard, hotkey, menu, system, ui};
use feathersend_core::backend::{
    Command, DeviceKind, DeviceView, IdentityView, IncomingRequest, NetworkHint, PairOutcome,
    Reply, SendProblem, TransferView, UiEvent,
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::UI::Controls::{NM_CLICK, NM_CUSTOMDRAW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    KillTimer, MINMAXINFO, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, SetTimer, WM_DROPFILES,
    WM_NOTIFY, WM_POWERBROADCAST,
};

pub enum AppEvent {
    Ui(UiEvent),
    Update(Release),
    OpenFolderFailed(PathBuf),
}

pub type Queue = Arc<Mutex<VecDeque<AppEvent>>>;

const HOTKEY_ID: i32 = 1;
const CLIP_TIMER: usize = 77;
const DEVICES_DROP_HANDLER: usize = 0x10_001;
const WM_GETMINMAXINFO: u32 = 0x0024;
const WM_HOTKEY: u32 = 0x0312;
const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const WM_TIMER: u32 = 0x0113;

/// Column weights: name, address, status.
const DEVICE_COLUMNS: [i32; 3] = [38, 28, 34];
/// Column weights: device, item, progress, state.
const TRANSFER_COLUMNS: [i32; 4] = [20, 34, 28, 18];
/// Width of the status bar's update notice, in logical units.
const UPDATE_PART_W: i32 = 330;

enum Prompt {
    Incoming(IncomingRequest, Reply<bool>),
    Text { from: String, text: String },
    Clipboard { from: String, content: ClipContent, reply: Reply<bool> },
    Pair { alias: String, code: String, reply: Reply<bool> },
    Info { text: String, error: bool },
    About,
}

pub struct Context {
    pub queue: Queue,
    pub settings: Settings,
    pub dirs: DataDirs,
    pub texts: &'static Texts,
    pub exe: PathBuf,
    pub minimized: bool,
}

/// What a click on the last balloon does.
#[derive(Clone, Copy)]
enum BalloonClick {
    OpenReceivedFolder,
    OpenUpdatePage,
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
    identity: Option<IdentityView>,
    update: Option<Release>,
    balloon_click: Option<BalloonClick>,
}

#[derive(Default)]
pub struct App {
    window: nwg::Window,
    icon: nwg::Icon,
    devices_label: nwg::Label,
    devices: nwg::ListView,
    device_images: nwg::ImageList,
    send_files: nwg::Button,
    /// Kept alive: the button shows this icon without copying it.
    send_icon: Option<ui::OwnedIcon>,
    send_folder: nwg::Button,
    send_clipboard: nwg::Button,
    pair: nwg::Button,
    add_ip: nwg::Button,
    open_folder: nwg::Button,
    settings_button: nwg::Button,
    about: nwg::Button,
    bold_font: nwg::Font,
    transfers_label: nwg::Label,
    transfers: nwg::ListView,
    cancel: nwg::Button,
    resume: nwg::Button,
    status: nwg::StatusBar,
    tray: nwg::TrayNotification,
    notice: nwg::Notice,
    opening_folder: Arc<AtomicBool>,
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
            .size((layout::DEFAULT_CLIENT.0 as i32, layout::DEFAULT_CLIENT.1 as i32))
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
                .double_buffer(false)
                .ex_flags(nwg::ListViewExFlags::FULL_ROW_SELECT)
                .flags(
                    nwg::ListViewFlags::VISIBLE
                        | nwg::ListViewFlags::SINGLE_SELECTION
                        | nwg::ListViewFlags::TAB_STOP,
                )
                .build(list)?;
            ui::modern_list(win::hwnd_of(&list.handle));
        }
        app.devices.set_headers_enabled(true);
        app.transfers.set_headers_enabled(true);
        // Real widths come from fit_columns once the lists have their size.
        for (i, text) in [t.col_name, t.col_address, t.col_status].into_iter().enumerate() {
            column(&app.devices, i as i32, text, 100);
        }
        for (i, text) in
            [t.col_device, t.col_item, t.col_progress, t.col_state].into_iter().enumerate()
        {
            column(&app.transfers, i as i32, text, 100);
        }
        for (button, text) in [
            (&mut app.send_files, t.send_files),
            (&mut app.send_folder, t.send_folder),
            (&mut app.send_clipboard, t.send_clipboard),
            (&mut app.pair, t.pair),
            (&mut app.add_ip, t.add_by_ip),
            (&mut app.open_folder, t.open_received),
            (&mut app.settings_button, t.settings),
            (&mut app.about, t.about),
            (&mut app.cancel, t.cancel),
            (&mut app.resume, t.resume),
        ] {
            nwg::Button::builder().parent(w).text(text).build(button)?;
        }
        // Icons are drawn in code (see icons.rs), enlarged on high DPI.
        let factor = ui::icon_factor();
        let icon_size = icons::SIZE * factor;
        let device_icons: Vec<Vec<u32>> =
            icons::device_icons().iter().map(|p| icons::scaled(p, factor)).collect();
        app.device_images = ui::image_list(&device_icons, icon_size);
        app.devices.set_image_list(Some(&app.device_images), nwg::ListViewImageListType::Small);
        // The primary action stands out: taller (see layout), bold, with an arrow.
        if let Some(icon) = ui::create_icon(&icons::scaled(&icons::send_icon(), factor), icon_size)
        {
            let mut image = nwg::Icon::default(); // not owned: `icon` destroys it
            image.handle = icon.handle() as _;
            app.send_files.set_icon(Some(&image));
            app.send_icon = Some(icon);
        }
        if nwg::Font::builder()
            .family("Segoe UI")
            .size(16)
            .weight(700)
            .build(&mut app.bold_font)
            .is_ok()
        {
            app.send_files.set_font(Some(&app.bold_font));
        }
        nwg::StatusBar::builder().parent(w).build(&mut app.status)?;
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
        app.update_actions();
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
                        info.ptMinTrackSize.x = ui::px(layout::MIN_WINDOW.0);
                        info.ptMinTrackSize.y = ui::px(layout::MIN_WINDOW.1);
                        Some(0)
                    }
                    // A drop outside the device list. Handled here, before
                    // nwg, whose OnFileDrop leaks the drop handle.
                    WM_DROPFILES => {
                        let (paths, _) = unsafe { ui::take_drop(wparam) };
                        app.files_dropped(paths, None);
                        Some(0)
                    }
                    WM_NOTIFY => {
                        let (from, code) = unsafe { ui::notify_header(lparam) };
                        if code == NM_CUSTOMDRAW {
                            let t = app.t();
                            let hint = if from == win::hwnd_of(&app.devices.handle) {
                                (app.devices.len() == 0).then_some(t.empty_devices)
                            } else if from == win::hwnd_of(&app.transfers.handle) {
                                (app.transfers.len() == 0).then_some(t.empty_transfers)
                            } else {
                                None
                            };
                            return hint.map(|hint| unsafe { ui::draw_empty_hint(lparam, hint) });
                        }
                        if code == NM_CLICK
                            && from == win::hwnd_of(&app.status.handle)
                            && unsafe { ui::clicked_part(lparam) } == 1
                        {
                            app.open_update_page();
                        }
                        None
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
                    // Sockets bound before sleeping may be dead after it.
                    WM_POWERBROADCAST
                        if matches!(
                            wparam as u32,
                            PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND
                        ) =>
                    {
                        app.send(Command::NetworkChanged(NetworkHint::Resume));
                        None
                    }
                    _ => None,
                }
            },
        );
        // A drop on the device list, with the row under the cursor.
        ui::accept_files(win::hwnd_of(&self.devices.handle));
        let weak = Rc::downgrade(self);
        let _ = nwg::bind_raw_event_handler(
            &self.devices.handle,
            DEVICES_DROP_HANDLER,
            move |hwnd, msg, wparam, _lparam| {
                if msg != WM_DROPFILES {
                    return None;
                }
                let app = weak.upgrade()?;
                let (paths, point) = unsafe { ui::take_drop(wparam) };
                app.files_dropped(paths, ui::list_row_at(hwnd as _, point));
                Some(0)
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

    /// Lays out every control. Uses `ui::place`, never nwg's position and size
    /// calls: see `win::ui` about their GDI handle leak.
    fn layout(&self) {
        let (w, h) = ui::client_size(self.hwnd());
        if w == 0 || h == 0 {
            return; // minimized
        }
        let status = win::hwnd_of(&self.status.handle);
        let l = layout::main_layout(ui::logical(w), ui::logical(h - ui::window_height(status)));
        // Identity on the left; the update notice appears on the right.
        let update_w = if self.state.borrow().update.is_some() { ui::px(UPDATE_PART_W) } else { 0 };
        ui::set_status_parts(status, &[w - update_w, -1]);
        let moves: Vec<_> = [
            (self.devices_label.handle, l.devices_label),
            (self.devices.handle, l.devices),
            (self.transfers_label.handle, l.transfers_label),
            (self.transfers.handle, l.transfers),
            (self.send_files.handle, l.send_files),
            (self.send_folder.handle, l.send_folder),
            (self.send_clipboard.handle, l.send_clipboard),
            (self.pair.handle, l.pair),
            (self.add_ip.handle, l.add_ip),
            (self.cancel.handle, l.cancel),
            (self.resume.handle, l.resume),
            (self.open_folder.handle, l.open_folder),
            (self.settings_button.handle, l.settings),
            (self.about.handle, l.about),
        ]
        .into_iter()
        .map(|(handle, r)| {
            (win::hwnd_of(&handle), [ui::px(r.x), ui::px(r.y), ui::px(r.w), ui::px(r.h)])
        })
        .collect();
        ui::place(&moves);
        self.fit_columns();
    }

    /// Columns exactly fill each list (no horizontal scroll bar); the client
    /// width already excludes the vertical scroll bar when there is one.
    fn fit_columns(&self) {
        for (list, weights) in
            [(&self.devices, &DEVICE_COLUMNS[..]), (&self.transfers, &TRANSFER_COLUMNS[..])]
        {
            let (width, _) = ui::client_size(win::hwnd_of(&list.handle));
            for (i, w) in view::column_widths(width, weights).into_iter().enumerate() {
                list.set_column_width(i, w as isize);
            }
            // The empty-list hint is centered: repaint it when the size changes.
            if list.len() == 0 {
                list.invalidate();
            }
        }
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
            // nwg reports a maximize only as OnWindowMaximize, never as OnResize.
            E::OnResize | E::OnWindowMaximize if handle == self.window.handle => self.layout(),
            E::OnNotice if handle == self.notice.handle => self.drain(),
            E::OnButtonClick => self.on_button(handle),
            E::OnListViewRightClick if handle == self.devices.handle => self.device_menu(),
            E::OnListViewDoubleClick if handle == self.devices.handle => {
                if let nwg::EventData::OnListViewItemIndex { row_index, .. } = data {
                    self.device_double_click(row_index);
                }
            }
            E::OnListViewItemChanged
                if handle == self.devices.handle || handle == self.transfers.handle =>
            {
                self.update_actions()
            }
            E::OnContextMenu if handle == self.tray.handle => self.tray_menu(),
            E::OnTrayNotificationUserClose if handle == self.tray.handle => {
                let click = self.state.borrow_mut().balloon_click.take();
                match click {
                    Some(BalloonClick::OpenReceivedFolder) => self.open_received_folder(),
                    Some(BalloonClick::OpenUpdatePage) => self.open_update_page(),
                    None => {}
                }
            }
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
        } else if handle == self.open_folder.handle {
            self.open_received_folder();
        } else if handle == self.settings_button.handle {
            self.settings_action();
        } else if handle == self.about.handle {
            self.queue_prompt(Prompt::About);
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

    /// On its own thread: a dead network share would freeze the window.
    fn open_received_folder(&self) {
        if self.opening_folder.swap(true, Ordering::Relaxed) {
            return;
        }
        let path = self.settings.borrow().download_dir.clone();
        let busy = self.opening_folder.clone();
        let queue = self.ctx.as_ref().expect("context").queue.clone();
        let notice = self.notice.sender();
        std::thread::spawn(move || {
            let result = system::open_folder(&path);
            busy.store(false, Ordering::Relaxed);
            if let Err(e) = result {
                tracing::warn!("cannot open {}: {e}", path.display());
                queue.lock().unwrap().push_back(AppEvent::OpenFolderFailed(path));
                notice.notice();
            }
        });
    }

    fn refresh_status(&self) {
        let t = self.t();
        let state = self.state.borrow();
        if let Some(identity) = &state.identity {
            self.status.set_text(0, &view::identity_text(t, identity, false));
        }
        if let Some(release) = &state.update {
            let version = release.version.to_string();
            self.status.set_text(1, &fill(t.update_status, &[("version", &version)]));
        }
    }

    fn open_update_page(&self) {
        // No borrow across the shell call: it can pump messages and re-enter drain().
        let url = self.state.borrow().update.as_ref().map(|release| release.url.clone());
        if let Some(url) = url {
            system::open_url(&url);
        }
    }

    fn balloon(&self, text: &str) {
        self.balloon_then(text, None);
    }

    fn balloon_then(&self, text: &str, click: Option<BalloonClick>) {
        self.state.borrow_mut().balloon_click = click;
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
        let list: Vec<rows::Row> = state
            .devices
            .iter()
            .map(|d| rows::Row {
                image: Some(icons::device_icon_index(d)),
                cells: vec![
                    d.alias.clone(),
                    d.address.clone().unwrap_or_default(),
                    view::device_status(t, d),
                ],
            })
            .collect();
        let row = selected.and_then(|fp| state.devices.iter().position(|d| d.fingerprint == fp));
        drop(state);
        rows::sync(&self.devices, &list, row);
        self.fit_columns();
        self.update_actions();
    }

    /// Enables the buttons that make sense for the selected rows.
    fn update_actions(&self) {
        let device = self.selected_device();
        let transfer = self
            .transfers
            .selected_item()
            .and_then(|i| self.state.borrow().transfers.get(i).cloned());
        let actions = view::Actions::of(device.as_ref(), transfer.as_ref());
        self.send_files.set_enabled(actions.send);
        self.send_folder.set_enabled(actions.send);
        self.send_clipboard.set_enabled(actions.send);
        self.pair.set_enabled(actions.pair);
        self.cancel.set_enabled(actions.cancel);
        self.resume.set_enabled(actions.resume);
        let focus = ui::focused();
        let on_transfer_button = focus == win::hwnd_of(&self.cancel.handle)
            || focus == win::hwnd_of(&self.resume.handle);
        let list = if on_transfer_button { &self.transfers } else { &self.devices };
        ui::rescue_focus(win::hwnd_of(&list.handle));
    }

    fn tell_offline(self: &Rc<Self>, alias: &str) {
        let t = self.t();
        let text = format!("{}{alias} ({})", t.error_prefix, t.status_offline);
        self.queue_prompt(Prompt::Info { text, error: false });
    }

    /// Double-click on a row sends files to it; on empty space, nothing.
    fn device_double_click(self: &Rc<Self>, row: usize) {
        let Some(d) = self.state.borrow().devices.get(row).cloned() else { return };
        if d.online {
            self.send_files_action();
        } else {
            self.tell_offline(&d.alias);
        }
    }

    fn device_menu(self: &Rc<Self>) {
        let t = self.t();
        let Some(d) = self.selected_device() else { return };
        let paired = d.kind == DeviceKind::Paired;
        use menu::Item;
        let chosen = menu::popup(
            self.hwnd(),
            &[
                Item::check(1, t.send_files, false, d.online),
                Item::check(2, t.send_folder, false, d.online),
                Item::check(3, t.send_clipboard, false, d.online),
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
                Item::new(5, t.open_received),
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
            5 => self.open_received_folder(),
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
                self.choose_device(&online)
            }
        };
        if let Some(device) = target {
            self.send(Command::SendClipboard { device: device.fingerprint, content });
        }
    }

    /// A popup menu of `devices` at the cursor.
    fn choose_device(&self, devices: &[DeviceView]) -> Option<DeviceView> {
        let items: Vec<menu::Item> = devices
            .iter()
            .enumerate()
            .map(|(i, d)| menu::Item::new(100 + i as u32, &d.alias))
            .collect();
        let chosen = menu::popup(self.hwnd(), &items);
        chosen.checked_sub(100).and_then(|i| devices.get(i as usize).cloned())
    }

    /// Files dropped on a device row (`hit`) or anywhere else in the window.
    fn files_dropped(self: &Rc<Self>, paths: Vec<PathBuf>, hit: Option<usize>) {
        if paths.is_empty() {
            return;
        }
        let target = {
            let state = self.state.borrow();
            view::drop_target(&state.devices, hit, self.devices.selected_item())
        };
        let device = match target {
            view::DropTarget::Device(device) => Some(device),
            view::DropTarget::Choose(online) => self.choose_device(&online),
            view::DropTarget::Offline(alias) => {
                self.tell_offline(&alias);
                None
            }
            view::DropTarget::NoneOnline => {
                let text = self.t().none_online.into();
                self.queue_prompt(Prompt::Info { text, error: false });
                None
            }
        };
        if let Some(device) = device {
            // Shows where the files went, and targets the next drop.
            let row = {
                let state = self.state.borrow();
                state.devices.iter().position(|d| d.fingerprint == device.fingerprint)
            };
            if let Some(row) = row {
                self.devices.select_item(row, true);
            }
            self.send(Command::SendFiles { device: device.fingerprint, paths });
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
        let list: Vec<rows::Row> = state
            .transfers
            .iter()
            .map(|v| rows::Row { image: None, cells: view::transfer_cells(t, v).into() })
            .collect();
        let row = selected.and_then(|id| state.transfers.iter().position(|v| v.id == id));
        drop(state);
        rows::sync(&self.transfers, &list, row);
        self.fit_columns();
        self.update_actions();
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
                AppEvent::Update(release) => {
                    let version = release.version.to_string();
                    self.state.borrow_mut().update = Some(release);
                    self.layout();
                    self.refresh_status();
                    let text = fill(self.t().update_status, &[("version", &version)]);
                    self.balloon_then(&text, Some(BalloonClick::OpenUpdatePage));
                }
                AppEvent::OpenFolderFailed(path) => {
                    let path = path.display().to_string();
                    let text = fill(self.t().open_folder_failed, &[("path", &path)]);
                    self.queue_prompt(Prompt::Info { text, error: true });
                }
                AppEvent::Ui(event) => match event {
                    UiEvent::Devices(list) => {
                        self.state.borrow_mut().devices = list;
                        devices_changed = true;
                    }
                    UiEvent::Identity(identity) => {
                        self.state.borrow_mut().identity = Some(identity);
                        self.refresh_status();
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
                        self.balloon_then(&text, Some(BalloonClick::OpenReceivedFolder));
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
            Prompt::About => {
                let (identity, update) = {
                    let state = self.state.borrow();
                    (state.identity.clone(), state.update.clone())
                };
                let mut body = fill(t.about_body, &[("version", &Version::current().to_string())]);
                if let Some(identity) = &identity {
                    body.push_str("\n\n");
                    body.push_str(&view::identity_text(t, identity, true));
                }
                match update {
                    Some(release) => {
                        let version = release.version.to_string();
                        body.push_str("\n\n");
                        body.push_str(&fill(t.update_available, &[("version", &version)]));
                        if self.yes_no("FeatherSend", &body) {
                            system::open_url(&release.url);
                        }
                    }
                    None => {
                        nwg::modal_info_message(&self.window, "FeatherSend", &body);
                    }
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

#[cfg(test)]
mod text_fit_tests {
    use super::*;
    use feathersend_core::i18n::Language;
    use windows_sys::Win32::Foundation::SIZE;
    use windows_sys::Win32::Graphics::Gdi::{
        GetDC, GetTextExtentPoint32W, ReleaseDC, SelectObject,
    };

    fn font(weight: u32) -> nwg::Font {
        let mut font = nwg::Font::default();
        nwg::Font::builder().family("Segoe UI").size(16).weight(weight).build(&mut font).unwrap();
        font
    }

    fn width(font: &nwg::Font, text: &str) -> i32 {
        let wide: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        unsafe {
            let dc = GetDC(std::ptr::null_mut());
            let old = SelectObject(dc, font.handle as _);
            GetTextExtentPoint32W(dc, wide.as_ptr(), wide.len() as i32, &mut size);
            SelectObject(dc, old);
            ReleaseDC(std::ptr::null_mut(), dc);
        }
        size.cx
    }

    /// Control sizes in logical units, with room for the control's own edges.
    #[test]
    fn every_label_fits_its_control_in_every_language() {
        let _guard = ui::NWG_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        nwg::init().unwrap();
        let (regular, bold) = (font(400), font(700));
        let side = layout::SIDE_W - 12;
        let mut overflow = Vec::new();
        for language in Language::ALL {
            let t = language.texts();
            let checks = [
                (&bold, t.send_files, side),
                (&regular, t.send_folder, side),
                (&regular, t.send_clipboard, side),
                (&regular, t.pair, side),
                (&regular, t.add_by_ip, side),
                (&regular, t.cancel, side),
                (&regular, t.resume, side),
                (&regular, t.open_received, side),
                (&regular, t.settings, side),
                (&regular, t.about, side),
                (&regular, t.alias_label, 170),
                (&regular, t.download_dir_label, 170),
                (&regular, t.language_label, 170),
                (&regular, t.hotkey_label, 170),
                (&regular, t.browse, 88 - 12),
                (&regular, t.save, 92 - 12),
                (&regular, t.cancel, 92 - 12),
                (&regular, t.start_with_windows, 512 - 20),
                (&regular, t.check_updates, 512 - 20),
                (&regular, t.accept_unpaired, 512 - 20),
                (&regular, t.update_status, UPDATE_PART_W - 16),
            ];
            for (font, text, room) in checks {
                let needed = width(font, text);
                if needed > ui::px(room) {
                    overflow.push(format!(
                        "{}: {text} ({needed} > {})",
                        language.code(),
                        ui::px(room)
                    ));
                }
            }
        }
        assert!(overflow.is_empty(), "{}", overflow.join("\n"));
    }
}
