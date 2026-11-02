//! The settings window. Built once, hidden and refreshed on reuse.

use feathersend_core::i18n::{Language, Texts};
use feathersend_core::settings::Settings;
use native_windows_gui as nwg;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use windows_sys::Win32::UI::WindowsAndMessaging::{IDCANCEL, WM_COMMAND};

const KEYS_HANDLER: usize = 0x10_002;

type OnSave = Box<dyn Fn(Settings)>;

/// The client area: the controls end 12 units before its right and bottom edges.
const CLIENT: (u32, u32) = (536, 342);

/// The language list starts with "Automatic", then `Language::ALL`.
fn language_index(code: &str) -> usize {
    Language::ALL.iter().position(|l| l.code().eq_ignore_ascii_case(code)).map_or(0, |i| i + 1)
}

fn language_code(index: usize) -> String {
    index
        .checked_sub(1)
        .and_then(|i| Language::ALL.get(i))
        .map_or("auto", |language| language.code())
        .to_string()
}

#[derive(Default)]
pub struct SettingsWindow {
    pub window: nwg::Window,
    alias_label: nwg::Label,
    alias: nwg::TextInput,
    dir_label: nwg::Label,
    dir: nwg::TextInput,
    browse: nwg::Button,
    lang_label: nwg::Label,
    lang: nwg::ComboBox<String>,
    hotkey_label: nwg::Label,
    hotkey: nwg::TextInput,
    start_with_windows: nwg::CheckBox,
    check_updates: nwg::CheckBox,
    accept_unpaired: nwg::CheckBox,
    save: nwg::Button,
    cancel: nwg::Button,
    folder_dialog: nwg::FileDialog,
    current: RefCell<Settings>,
    on_save: RefCell<Option<OnSave>>,
    texts: Option<&'static Texts>,
}

fn check(b: bool) -> nwg::CheckBoxState {
    if b { nwg::CheckBoxState::Checked } else { nwg::CheckBoxState::Unchecked }
}

impl SettingsWindow {
    pub fn build(parent: &nwg::Window, icon: &nwg::Icon, t: &'static Texts) -> Rc<SettingsWindow> {
        let mut w = SettingsWindow { texts: Some(t), ..Default::default() };
        nwg::Window::builder()
            .title(t.settings_title)
            .size((CLIENT.0 as i32, CLIENT.1 as i32))
            .center(true)
            .icon(Some(icon))
            .parent(Some(parent))
            .flags(nwg::WindowFlags::WINDOW)
            .build(&mut w.window)
            .expect("settings window");
        // The builder's size includes the title bar and borders; this one does not.
        w.window.set_size(CLIENT.0, CLIENT.1);
        let label = |out: &mut nwg::Label, text: &str, y: i32| {
            nwg::Label::builder()
                .parent(&w.window)
                .text(text)
                .position((12, y))
                .size((170, 22))
                .build(out)
                .unwrap();
        };
        label(&mut w.alias_label, t.alias_label, 16);
        label(&mut w.dir_label, t.download_dir_label, 52);
        label(&mut w.lang_label, t.language_label, 88);
        label(&mut w.hotkey_label, t.hotkey_label, 124);
        nwg::TextInput::builder()
            .parent(&w.window)
            .position((190, 14))
            .size((334, 24))
            .limit(64)
            .build(&mut w.alias)
            .unwrap();
        nwg::TextInput::builder()
            .parent(&w.window)
            .position((190, 50))
            .size((240, 24))
            .build(&mut w.dir)
            .unwrap();
        nwg::Button::builder()
            .parent(&w.window)
            .text(t.browse)
            .position((436, 49))
            .size((88, 26))
            .build(&mut w.browse)
            .unwrap();
        nwg::ComboBox::builder()
            .parent(&w.window)
            .position((190, 86))
            .size((200, 24))
            .collection(
                std::iter::once(t.language_auto.to_string())
                    .chain(Language::ALL.iter().map(|l| l.native_name().to_string()))
                    .collect(),
            )
            .build(&mut w.lang)
            .unwrap();
        nwg::TextInput::builder()
            .parent(&w.window)
            .position((190, 122))
            .size((200, 24))
            .build(&mut w.hotkey)
            .unwrap();
        let checkbox = |out: &mut nwg::CheckBox, text: &str, y: i32| {
            nwg::CheckBox::builder()
                .parent(&w.window)
                .text(text)
                .position((12, y))
                .size((512, 24))
                .build(out)
                .unwrap();
        };
        checkbox(&mut w.start_with_windows, t.start_with_windows, 164);
        checkbox(&mut w.check_updates, t.check_updates, 194);
        checkbox(&mut w.accept_unpaired, t.accept_unpaired, 224);
        nwg::Button::builder()
            .parent(&w.window)
            .text(t.save)
            .position((334, 300))
            .size((92, 30))
            .build(&mut w.save)
            .unwrap();
        nwg::Button::builder()
            .parent(&w.window)
            .text(t.cancel)
            .position((432, 300))
            .size((92, 30))
            .build(&mut w.cancel)
            .unwrap();
        nwg::FileDialog::builder()
            .action(nwg::FileDialogAction::OpenDirectory)
            .title(t.download_dir_label)
            .build(&mut w.folder_dialog)
            .unwrap();

        let w = Rc::new(w);
        // Esc reaches the window as IDCANCEL: nwg's message loop runs
        // IsDialogMessage, which keeps the key from the focused field.
        let weak = Rc::downgrade(&w);
        let _ = nwg::bind_raw_event_handler(
            &w.window.handle,
            KEYS_HANDLER,
            move |_hwnd, msg, wparam, _lparam| {
                if msg != WM_COMMAND || wparam & 0xFFFF != IDCANCEL as usize {
                    return None;
                }
                weak.upgrade()?.window.set_visible(false);
                Some(0)
            },
        );
        let weak = Rc::downgrade(&w);
        nwg::full_bind_event_handler(&w.window.handle, move |event, data, handle| {
            let Some(w) = weak.upgrade() else { return };
            match event {
                nwg::Event::OnWindowClose => {
                    if let nwg::EventData::OnWindowClose(close) = data {
                        close.close(false);
                    }
                    w.window.set_visible(false);
                }
                nwg::Event::OnButtonClick if handle == w.cancel.handle => {
                    w.window.set_visible(false)
                }
                nwg::Event::OnButtonClick if handle == w.browse.handle => {
                    if w.folder_dialog.run(Some(&w.window))
                        && let Ok(path) = w.folder_dialog.get_selected_item()
                    {
                        w.dir.set_text(&path.to_string_lossy());
                    }
                }
                nwg::Event::OnButtonClick if handle == w.save.handle => w.on_save_clicked(),
                _ => {}
            }
        });
        w
    }

    pub fn open(&self, settings: &Settings, autostart: bool, on_save: OnSave) {
        *self.current.borrow_mut() = settings.clone();
        *self.on_save.borrow_mut() = Some(on_save);
        self.alias.set_text(&settings.alias);
        self.dir.set_text(&settings.download_dir.to_string_lossy());
        self.lang.set_selection(Some(language_index(&settings.language)));
        self.hotkey.set_text(&settings.clipboard_hotkey);
        self.start_with_windows.set_check_state(check(autostart));
        self.check_updates.set_check_state(check(settings.check_updates));
        self.accept_unpaired.set_check_state(check(settings.accept_from_unpaired));
        self.window.set_visible(true);
        self.alias.set_focus();
    }

    fn on_save_clicked(&self) {
        let t = self.texts.expect("texts");
        let hotkey = self.hotkey.text().trim().to_string();
        if crate::win::hotkey::parse(&hotkey).is_none() {
            nwg::modal_error_message(&self.window, "FeatherSend", t.invalid_hotkey);
            return;
        }
        let mut settings = self.current.borrow().clone();
        let alias = self.alias.text().trim().to_string();
        if !alias.is_empty() {
            settings.alias = alias;
        }
        let dir = self.dir.text().trim().to_string();
        if !dir.is_empty() {
            settings.download_dir = PathBuf::from(dir);
        }
        settings.language = language_code(self.lang.selection().unwrap_or(0));
        settings.clipboard_hotkey = hotkey;
        settings.start_with_windows =
            self.start_with_windows.check_state() == nwg::CheckBoxState::Checked;
        settings.check_updates = self.check_updates.check_state() == nwg::CheckBoxState::Checked;
        settings.accept_from_unpaired =
            self.accept_unpaired.check_state() == nwg::CheckBoxState::Checked;
        self.window.set_visible(false);
        if let Some(on_save) = self.on_save.borrow().as_ref() {
            on_save(settings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_sits_inside_the_window_with_a_margin() {
        use crate::win::{hwnd_of, ui};
        use windows_sys::Win32::Foundation::{POINT, RECT};
        use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

        let _guard = ui::NWG_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        nwg::init().unwrap();
        let mut parent = nwg::Window::default();
        nwg::Window::builder().flags(nwg::WindowFlags::WINDOW).build(&mut parent).unwrap();
        let icon = nwg::Icon::default();
        let w = SettingsWindow::build(&parent, &icon, Language::It.texts());

        let window = hwnd_of(&w.window.handle);
        let (client_w, client_h) = ui::client_size(window);
        let margin = ui::px(8);
        for (name, handle) in [
            ("alias", &w.alias.handle),
            ("dir", &w.dir.handle),
            ("browse", &w.browse.handle),
            ("lang", &w.lang.handle),
            ("hotkey", &w.hotkey.handle),
            ("start_with_windows", &w.start_with_windows.handle),
            ("check_updates", &w.check_updates.handle),
            ("accept_unpaired", &w.accept_unpaired.handle),
            ("save", &w.save.handle),
            ("cancel", &w.cancel.handle),
        ] {
            let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
            let mut corner = POINT { x: 0, y: 0 };
            unsafe {
                GetWindowRect(hwnd_of(handle), &mut rect);
                corner.x = rect.right;
                corner.y = rect.bottom;
                ScreenToClient(window, &mut corner);
            }
            assert!(corner.x <= client_w - margin, "{name}: right {} of {client_w}", corner.x);
            assert!(corner.y <= client_h - margin, "{name}: bottom {} of {client_h}", corner.y);
        }
    }

    #[test]
    fn escape_closes_the_window() {
        use crate::win::{hwnd_of, ui};
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            IsDialogMessageW, IsWindowVisible, MSG, SW_SHOWNA, ShowWindow, WM_KEYDOWN,
        };

        let _guard = ui::NWG_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        nwg::init().unwrap();
        let mut parent = nwg::Window::default();
        nwg::Window::builder().flags(nwg::WindowFlags::WINDOW).build(&mut parent).unwrap();
        let icon = nwg::Icon::default();
        let w = SettingsWindow::build(&parent, &icon, Language::It.texts());
        let window = hwnd_of(&w.window.handle);
        // Off screen and not activated: this runs on a desktop someone is using.
        w.window.set_position(-4000, -4000);
        unsafe { ShowWindow(window, SW_SHOWNA) };
        assert_ne!(unsafe { IsWindowVisible(window) }, 0);

        // What nwg's message loop does with the key while a field has the focus.
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        msg.hwnd = hwnd_of(&w.alias.handle);
        msg.message = WM_KEYDOWN;
        msg.wParam = VK_ESCAPE as usize;
        unsafe { IsDialogMessageW(window, &msg) };

        assert_eq!(unsafe { IsWindowVisible(window) }, 0);
    }

    #[test]
    fn language_list_round_trips_every_code() {
        assert_eq!(language_code(language_index("auto")), "auto");
        for language in Language::ALL {
            assert_eq!(language_code(language_index(language.code())), language.code());
        }
        assert_eq!(language_index("unknown"), 0);
        assert_eq!(language_code(99), "auto");
    }
}
