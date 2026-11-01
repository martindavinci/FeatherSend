//! The settings window. Built once, hidden and refreshed on reuse.

use feathersend_core::i18n::{Language, Texts};
use feathersend_core::settings::Settings;
use native_windows_gui as nwg;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

type OnSave = Box<dyn Fn(Settings)>;

const LANG_CODES: [&str; 3] = ["auto", "en", "it"];

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
            .size((540, 360))
            .center(true)
            .icon(Some(icon))
            .parent(Some(parent))
            .flags(nwg::WindowFlags::WINDOW)
            .build(&mut w.window)
            .expect("settings window");
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
            .collection(vec![
                t.language_auto.to_string(),
                Language::En.native_name().to_string(),
                Language::It.native_name().to_string(),
            ])
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
            .position((336, 300))
            .size((92, 30))
            .build(&mut w.save)
            .unwrap();
        nwg::Button::builder()
            .parent(&w.window)
            .text(t.cancel)
            .position((434, 300))
            .size((92, 30))
            .build(&mut w.cancel)
            .unwrap();
        nwg::FileDialog::builder()
            .action(nwg::FileDialogAction::OpenDirectory)
            .title(t.download_dir_label)
            .build(&mut w.folder_dialog)
            .unwrap();

        let w = Rc::new(w);
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
        self.lang
            .set_selection(LANG_CODES.iter().position(|c| *c == settings.language).or(Some(0)));
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
        settings.language = LANG_CODES[self.lang.selection().unwrap_or(0).min(2)].to_string();
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
