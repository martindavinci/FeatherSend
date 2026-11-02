//! Interface languages and their texts. English is the reference language;
//! a missing translation is a compile error.

mod en;
mod it;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    En,
    It,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::En, Language::It];

    /// Code stored in settings.
    pub fn code(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::It => "it",
        }
    }

    pub fn from_code(code: &str) -> Option<Language> {
        Language::ALL.into_iter().find(|l| l.code().eq_ignore_ascii_case(code))
    }

    /// Maps a Windows/BCP 47 tag (`it-IT`, `en_US`) by its language part.
    pub fn from_locale(tag: &str) -> Option<Language> {
        let primary = tag.split(['-', '_']).next()?;
        Language::from_code(primary)
    }

    /// The language's name in itself.
    pub fn native_name(self) -> &'static str {
        match self {
            Language::En => "English",
            Language::It => "Italiano",
        }
    }

    pub fn texts(self) -> &'static Texts {
        match self {
            Language::En => &en::TEXTS,
            Language::It => &it::TEXTS,
        }
    }

    /// The setting (`"auto"`, `"en"`, `"it"`) if explicit, else the first
    /// supported system language, else English.
    pub fn resolve(setting: &str, system: &[String]) -> Language {
        Language::from_code(setting)
            .or_else(|| system.iter().find_map(|tag| Language::from_locale(tag)))
            .unwrap_or(Language::En)
    }
}

/// Replaces each `{name}` with its value.
pub fn fill(text: &str, args: &[(&str, &str)]) -> String {
    args.iter()
        .fold(text.to_string(), |acc, (name, value)| acc.replace(&format!("{{{name}}}"), value))
}

/// `1.5 MB`-style sizes (decimal units, one decimal above KB).
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", UNITS[unit]) }
}

/// Every text the interface shows. Placeholders are `{name}`.
pub struct Texts {
    pub devices: &'static str,
    pub empty_devices: &'static str,
    pub empty_transfers: &'static str,
    pub col_name: &'static str,
    pub col_address: &'static str,
    pub col_status: &'static str,
    pub status_paired: &'static str,
    pub status_feathersend: &'static str,
    pub status_localsend: &'static str,
    pub status_offline: &'static str,
    pub send_files: &'static str,
    pub send_folder: &'static str,
    pub send_clipboard: &'static str,
    pub pair: &'static str,
    pub unpair: &'static str,
    pub add_by_ip: &'static str,
    pub settings: &'static str,
    pub about: &'static str,
    pub transfers: &'static str,
    pub col_device: &'static str,
    pub col_item: &'static str,
    pub col_progress: &'static str,
    pub col_state: &'static str,
    pub cancel: &'static str,
    pub resume: &'static str,
    pub open_received: &'static str,
    pub state_waiting: &'static str,
    pub state_running: &'static str,
    pub state_done: &'static str,
    /// `{reason}`
    pub state_failed: &'static str,
    pub state_cancelled: &'static str,
    pub state_paused: &'static str,
    pub menu_auto_accept: &'static str,
    pub menu_auto_sync: &'static str,
    pub incoming_title: &'static str,
    /// `{device}`, `{count}`, `{size}`, `{names}`
    pub incoming_body: &'static str,
    /// `{n}`
    pub incoming_more: &'static str,
    pub message_title: &'static str,
    /// `{device}`, `{text}`
    pub message_body: &'static str,
    pub clipboard_title: &'static str,
    /// `{device}`, `{chars}`
    pub clipboard_text_body: &'static str,
    /// `{device}`, `{w}`, `{h}`
    pub clipboard_image_body: &'static str,
    /// `{device}`
    pub clipboard_received: &'static str,
    pub clipboard_empty: &'static str,
    pub clipboard_files_to_localsend: &'static str,
    pub pair_title: &'static str,
    /// `{device}`, `{code}`
    pub pair_body: &'static str,
    /// `{device}`
    pub pair_ok: &'static str,
    /// `{device}`
    pub pair_failed: &'static str,
    /// `{device}`
    pub pair_needs_feathersend: &'static str,
    /// `{device}`
    pub unpair_confirm: &'static str,
    pub select_device: &'static str,
    pub no_devices_online: &'static str,
    pub none_online: &'static str,
    pub add_ip_prompt: &'static str,
    pub invalid_ip: &'static str,
    /// `{item}`, `{device}`
    pub received_notice: &'static str,
    /// `{device}`
    pub sent_notice: &'static str,
    /// `{device}`
    pub declined: &'static str,
    /// `{device}`
    pub busy: &'static str,
    /// `{port}`
    pub port_in_use: &'static str,
    pub error_prefix: &'static str,
    pub settings_title: &'static str,
    pub alias_label: &'static str,
    pub download_dir_label: &'static str,
    pub language_label: &'static str,
    pub language_auto: &'static str,
    pub start_with_windows: &'static str,
    pub check_updates: &'static str,
    pub hotkey_label: &'static str,
    pub invalid_hotkey: &'static str,
    pub accept_unpaired: &'static str,
    pub browse: &'static str,
    pub save: &'static str,
    pub restart_needed: &'static str,
    pub tray_open: &'static str,
    pub tray_pause: &'static str,
    pub tray_quit: &'static str,
    /// `{version}`
    pub update_available: &'static str,
    /// `{version}`
    pub about_body: &'static str,
    pub ok: &'static str,
    /// `{path}`
    pub crash_message: &'static str,
    /// `{alias}`, `{addresses}`, `{port}`
    pub this_device: &'static str,
    pub no_network: &'static str,
    /// `{path}`
    pub open_folder_failed: &'static str,
    /// `{version}`
    pub update_status: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_setting() {
        assert_eq!(Language::resolve("it", &["en-US".into()]), Language::It);
    }

    #[test]
    fn resolve_auto_uses_system() {
        assert_eq!(Language::resolve("auto", &["de-DE".into(), "it-IT".into()]), Language::It);
    }

    #[test]
    fn resolve_falls_back_to_en() {
        assert_eq!(Language::resolve("auto", &["fr-FR".into()]), Language::En);
    }

    #[test]
    fn fill_replaces_all() {
        assert_eq!(fill("{a} and {b} and {a}", &[("a", "1"), ("b", "2")]), "1 and 2 and 1");
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(999), "999 B");
        assert_eq!(format_size(1_500_000), "1.5 MB");
    }

    #[test]
    fn placeholders_match_between_languages() {
        let placeholders = |s: &str| {
            let mut v: Vec<String> = s
                .split('{')
                .skip(1)
                .filter_map(|p| p.split('}').next())
                .map(String::from)
                .collect();
            v.sort();
            v
        };
        let (en, it) = (Language::En.texts(), Language::It.texts());
        for (a, b) in [
            (en.incoming_body, it.incoming_body),
            (en.message_body, it.message_body),
            (en.clipboard_text_body, it.clipboard_text_body),
            (en.clipboard_image_body, it.clipboard_image_body),
            (en.pair_body, it.pair_body),
            (en.state_failed, it.state_failed),
            (en.received_notice, it.received_notice),
            (en.port_in_use, it.port_in_use),
            (en.about_body, it.about_body),
            (en.update_available, it.update_available),
            (en.crash_message, it.crash_message),
            (en.this_device, it.this_device),
            (en.update_status, it.update_status),
        ] {
            assert_eq!(placeholders(a), placeholders(b), "{a}");
        }
    }
}
