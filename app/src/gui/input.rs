//! A one-line input dialog (used for "Add by IP"). Built once, then hidden
//! and reused: nwg cannot unbind a handler from inside itself.

use native_windows_gui as nwg;
use std::cell::RefCell;
use std::rc::Rc;

type OnOk = Box<dyn Fn(String)>;

#[derive(Default)]
pub struct InputWindow {
    pub window: nwg::Window,
    label: nwg::Label,
    input: nwg::TextInput,
    ok: nwg::Button,
    cancel: nwg::Button,
    on_ok: RefCell<Option<OnOk>>,
}

impl InputWindow {
    pub fn build(
        parent: &nwg::Window,
        icon: &nwg::Icon,
        ok_text: &str,
        cancel_text: &str,
    ) -> Rc<InputWindow> {
        let mut w = InputWindow::default();
        nwg::Window::builder()
            .title("FeatherSend")
            .size((380, 140))
            .center(true)
            .icon(Some(icon))
            .parent(Some(parent))
            .flags(nwg::WindowFlags::WINDOW)
            .build(&mut w.window)
            .expect("input window");
        nwg::Label::builder()
            .parent(&w.window)
            .position((12, 12))
            .size((356, 20))
            .build(&mut w.label)
            .unwrap();
        nwg::TextInput::builder()
            .parent(&w.window)
            .position((12, 36))
            .size((356, 24))
            .build(&mut w.input)
            .unwrap();
        nwg::Button::builder()
            .parent(&w.window)
            .text(ok_text)
            .position((196, 76))
            .size((84, 28))
            .build(&mut w.ok)
            .unwrap();
        nwg::Button::builder()
            .parent(&w.window)
            .text(cancel_text)
            .position((284, 76))
            .size((84, 28))
            .build(&mut w.cancel)
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
                nwg::Event::OnButtonClick if handle == w.ok.handle => {
                    let text = w.input.text();
                    w.window.set_visible(false);
                    if let Some(on_ok) = w.on_ok.borrow().as_ref() {
                        on_ok(text);
                    }
                }
                _ => {}
            }
        });
        w
    }

    pub fn ask(&self, prompt: &str, on_ok: OnOk) {
        self.label.set_text(prompt);
        self.input.set_text("");
        *self.on_ok.borrow_mut() = Some(on_ok);
        self.window.set_visible(true);
        self.input.set_focus();
    }
}
