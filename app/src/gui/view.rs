//! What the main window shows and enables, as plain functions of the
//! backend's views: no Win32 here, so it is unit tested.

use feathersend_core::backend::{
    DeviceKind, DeviceView, IdentityView, TransferState, TransferView,
};
use feathersend_core::i18n::{Texts, fill, format_size};

/// Which buttons make sense for the current selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actions {
    /// Send files / folder / clipboard: an online device is selected.
    pub send: bool,
    /// Pair: an online device that is not paired yet.
    pub pair: bool,
    /// Cancel: the selected transfer has not ended.
    pub cancel: bool,
    /// Resume: the selected transfer was interrupted and can continue.
    pub resume: bool,
}

impl Actions {
    pub fn of(device: Option<&DeviceView>, transfer: Option<&TransferView>) -> Actions {
        Actions {
            send: device.is_some_and(|d| d.online),
            pair: device.is_some_and(|d| d.online && d.kind != DeviceKind::Paired),
            cancel: transfer.is_some_and(|t| !t.state.is_final()),
            resume: transfer.is_some_and(|t| t.resumable && t.state == TransferState::Paused),
        }
    }
}

/// Splits `total` pixels by `weights`; the last column takes the rounding
/// rest, so the columns exactly fill the list and no scroll bar appears.
pub fn column_widths(total: i32, weights: &[i32]) -> Vec<i32> {
    let total = total.max(0);
    let sum = weights.iter().sum::<i32>().max(1);
    let mut widths: Vec<i32> = weights.iter().map(|w| total * w / sum).collect();
    let used: i32 = widths.iter().sum();
    if let Some(last) = widths.last_mut() {
        *last += total - used;
    }
    widths
}

/// Where files dropped on the window go.
#[derive(Clone, Debug, PartialEq)]
pub enum DropTarget {
    Device(DeviceView),
    /// Dropped on an offline device: its name, to tell the user.
    Offline(String),
    /// Several online devices and none chosen: ask with a menu.
    Choose(Vec<DeviceView>),
    NoneOnline,
}

/// `hit` is the row under the cursor (only drops on the device list have
/// one), `selected` the selected row.
pub fn drop_target(
    devices: &[DeviceView],
    hit: Option<usize>,
    selected: Option<usize>,
) -> DropTarget {
    if let Some(d) = hit.and_then(|i| devices.get(i)) {
        return if d.online {
            DropTarget::Device(d.clone())
        } else {
            DropTarget::Offline(d.alias.clone())
        };
    }
    if let Some(d) = selected.and_then(|i| devices.get(i)).filter(|d| d.online) {
        return DropTarget::Device(d.clone());
    }
    let online: Vec<DeviceView> = devices.iter().filter(|d| d.online).cloned().collect();
    match online.len() {
        0 => DropTarget::NoneOnline,
        1 => DropTarget::Device(online[0].clone()),
        _ => DropTarget::Choose(online),
    }
}

/// `This PC: name · 192.168.1.20 · port 53317`. Only the default-route
/// address unless `all` (the status bar is narrow; About shows them all).
pub fn identity_text(t: &Texts, id: &IdentityView, all: bool) -> String {
    let shown = if id.primary && !all { &id.addresses[..1] } else { &id.addresses[..] };
    let addresses = if shown.is_empty() {
        t.no_network.to_string()
    } else {
        shown.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ")
    };
    fill(
        t.this_device,
        &[("alias", &id.alias), ("addresses", &addresses), ("port", &id.port.to_string())],
    )
}

/// The device list's status column.
pub fn device_status(t: &Texts, d: &DeviceView) -> String {
    let kind = match d.kind {
        DeviceKind::Paired => t.status_paired,
        DeviceKind::FeatherSend => t.status_feathersend,
        DeviceKind::LocalSend => t.status_localsend,
    };
    if d.online { kind.to_string() } else { format!("{kind} ({})", t.status_offline) }
}

/// The transfer list's cells: device, item, progress, state.
pub fn transfer_cells(t: &Texts, v: &TransferView) -> [String; 4] {
    let percent = (v.done * 100).checked_div(v.total).unwrap_or(100);
    let state = match &v.state {
        TransferState::Waiting => t.state_waiting.to_string(),
        TransferState::Running => t.state_running.to_string(),
        TransferState::Done => t.state_done.to_string(),
        TransferState::Failed(reason) => fill(t.state_failed, &[("reason", reason)]),
        TransferState::Cancelled => t.state_cancelled.to_string(),
        TransferState::Paused => t.state_paused.to_string(),
    };
    [
        v.device.clone(),
        format!("{} {}", if v.incoming { "↓" } else { "↑" }, v.item),
        format!("{percent}% · {} / {}", format_size(v.done), format_size(v.total)),
        state,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use feathersend_core::backend::DeviceClass;
    use feathersend_core::i18n::Language;
    use std::net::Ipv4Addr;

    fn device(alias: &str, kind: DeviceKind, online: bool) -> DeviceView {
        DeviceView {
            fingerprint: alias.to_uppercase(),
            alias: alias.into(),
            address: online.then(|| "192.168.1.30:53317".into()),
            class: DeviceClass::Mobile,
            kind,
            feathersend: kind != DeviceKind::LocalSend,
            online,
            auto_accept: false,
            auto_sync: false,
        }
    }

    fn transfer(state: TransferState, resumable: bool) -> TransferView {
        TransferView {
            id: 1,
            device: "phone".into(),
            item: "a.jpg".into(),
            incoming: false,
            done: 50,
            total: 200,
            state,
            resumable,
        }
    }

    fn cancel_resume(t: &TransferView) -> (bool, bool) {
        let a = Actions::of(None, Some(t));
        (a.cancel, a.resume)
    }

    #[test]
    fn nothing_selected_enables_nothing() {
        let a = Actions::of(None, None);
        assert_eq!(a, Actions { send: false, pair: false, cancel: false, resume: false });
    }

    #[test]
    fn offline_device_cannot_be_sent_to() {
        let offline = device("pc", DeviceKind::Paired, false);
        assert!(!Actions::of(Some(&offline), None).send);
        assert!(!Actions::of(Some(&offline), None).pair);
    }

    #[test]
    fn pair_only_for_online_unpaired_devices() {
        let phone = device("phone", DeviceKind::LocalSend, true);
        let paired = device("pc", DeviceKind::Paired, true);
        assert!(Actions::of(Some(&phone), None).pair);
        assert!(Actions::of(Some(&phone), None).send);
        assert!(!Actions::of(Some(&paired), None).pair);
    }

    #[test]
    fn cancel_and_resume_follow_the_transfer_state() {
        assert_eq!(cancel_resume(&transfer(TransferState::Running, false)), (true, false));
        assert_eq!(cancel_resume(&transfer(TransferState::Paused, true)), (true, true));
        assert_eq!(cancel_resume(&transfer(TransferState::Paused, false)), (true, false));
        assert_eq!(cancel_resume(&transfer(TransferState::Done, false)), (false, false));
        assert_eq!(cancel_resume(&transfer(TransferState::Cancelled, false)), (false, false));
    }

    #[test]
    fn columns_fill_the_width_exactly() {
        for total in [0, 1, 517, 600, 1001] {
            let widths = column_widths(total, &[22, 38, 22, 18]);
            assert_eq!(widths.len(), 4);
            assert_eq!(widths.iter().sum::<i32>(), total, "{total}: {widths:?}");
            assert!(widths.iter().all(|w| *w >= 0));
        }
        assert_eq!(column_widths(600, &[1, 1, 1]), [200, 200, 200]);
        assert_eq!(column_widths(-5, &[1, 1]), [0, 0]);
    }

    #[test]
    fn drop_on_a_row_targets_that_device() {
        let list =
            [device("a", DeviceKind::LocalSend, true), device("b", DeviceKind::Paired, false)];
        assert_eq!(drop_target(&list, Some(0), Some(1)), DropTarget::Device(list[0].clone()));
        assert_eq!(drop_target(&list, Some(1), Some(0)), DropTarget::Offline("b".into()));
    }

    #[test]
    fn drop_elsewhere_uses_selection_then_the_only_online_device() {
        let list = [
            device("a", DeviceKind::LocalSend, true),
            device("b", DeviceKind::LocalSend, true),
            device("c", DeviceKind::Paired, false),
        ];
        assert_eq!(drop_target(&list, None, Some(1)), DropTarget::Device(list[1].clone()));
        assert_eq!(
            drop_target(&list, None, Some(2)),
            DropTarget::Choose(vec![list[0].clone(), list[1].clone()]),
            "an offline selection does not count"
        );
        assert_eq!(drop_target(&list[1..], None, None), DropTarget::Device(list[1].clone()));
        assert_eq!(drop_target(&list[2..], None, None), DropTarget::NoneOnline);
        assert_eq!(drop_target(&[], None, None), DropTarget::NoneOnline);
    }

    #[test]
    fn identity_shows_the_primary_address_or_all() {
        let t = Language::En.texts();
        let id = IdentityView {
            alias: "OFFICE-PC".into(),
            addresses: vec![Ipv4Addr::new(192, 168, 1, 20), Ipv4Addr::new(192, 168, 56, 1)],
            primary: true,
            port: 53317,
        };
        assert_eq!(identity_text(t, &id, false), "This PC: OFFICE-PC · 192.168.1.20 · port 53317");
        assert_eq!(
            identity_text(t, &id, true),
            "This PC: OFFICE-PC · 192.168.1.20, 192.168.56.1 · port 53317"
        );
        let unknown = IdentityView { primary: false, ..id.clone() };
        assert!(identity_text(t, &unknown, false).contains("192.168.1.20, 192.168.56.1"));
        let offline = IdentityView { addresses: vec![], primary: false, ..id };
        assert_eq!(
            identity_text(t, &offline, false),
            "This PC: OFFICE-PC · no network · port 53317"
        );
    }

    #[test]
    fn status_and_progress_texts() {
        let t = Language::En.texts();
        assert_eq!(device_status(t, &device("a", DeviceKind::Paired, false)), "Paired (offline)");
        assert_eq!(device_status(t, &device("a", DeviceKind::LocalSend, true)), "LocalSend");
        let cells = transfer_cells(t, &transfer(TransferState::Running, false));
        assert_eq!(cells[1], "↑ a.jpg");
        assert_eq!(cells[2], "25% · 50 B / 200 B");
        assert_eq!(cells[3], "In progress");
    }
}
