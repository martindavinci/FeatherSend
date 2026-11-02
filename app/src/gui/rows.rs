//! Keeps a list view in step with its rows.

use crate::win::ui;
use native_windows_gui as nwg;

pub struct Row {
    pub image: Option<i32>,
    pub cells: Vec<String>,
}

/// Makes `list` show `rows` with row `selected` selected. Rows are updated in
/// place: a cleared list forgets its scroll position and keyboard position,
/// and transfers update it several times a second.
pub fn sync(list: &nwg::ListView, rows: &[Row], selected: Option<usize>) {
    list.set_redraw(false);
    for row in (rows.len()..list.len()).rev() {
        list.remove_item(row);
    }
    let existing = list.len();
    for (index, row) in rows.iter().enumerate() {
        for (col, text) in row.cells.iter().enumerate() {
            let item = nwg::InsertListViewItem {
                index: Some(index as i32),
                column_index: col as i32,
                text: Some(text.clone()),
                image: if col == 0 { row.image } else { None },
            };
            if index >= existing && col == 0 {
                list.insert_item(item);
            } else {
                list.update_item(index, item);
            }
        }
    }
    ui::list_select(list.handle.hwnd().expect("list") as _, selected);
    list.set_redraw(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Controls::{
        LVM_ENSUREVISIBLE, LVM_GETNEXTITEM, LVM_GETSELECTEDCOUNT, LVM_GETTOPINDEX, LVNI_FOCUSED,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW;

    struct Fixture {
        _guard: MutexGuard<'static, ()>,
        _window: nwg::Window,
        list: nwg::ListView,
    }

    /// A real list control in a hidden window: the bugs live in its behavior.
    fn fixture() -> Fixture {
        let guard = ui::NWG_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        nwg::init().unwrap();
        let mut window = nwg::Window::default();
        nwg::Window::builder()
            .flags(nwg::WindowFlags::WINDOW)
            .size((400, 300))
            .build(&mut window)
            .unwrap();
        let mut list = nwg::ListView::default();
        nwg::ListView::builder()
            .parent(&window)
            .size((300, 200))
            .list_style(nwg::ListViewStyle::Detailed)
            .flags(nwg::ListViewFlags::VISIBLE | nwg::ListViewFlags::SINGLE_SELECTION)
            .build(&mut list)
            .unwrap();
        for col in 0..2 {
            list.insert_column(nwg::InsertListViewColumn {
                index: Some(col),
                fmt: None,
                width: Some(100),
                text: Some(format!("c{col}")),
            });
        }
        Fixture { _guard: guard, _window: window, list }
    }

    fn rows(count: usize, tag: &str) -> Vec<Row> {
        (0..count)
            .map(|i| Row {
                image: Some(i as i32 % 2),
                cells: vec![format!("{tag}{i}"), format!("{tag}{i}b")],
            })
            .collect()
    }

    fn hwnd(list: &nwg::ListView) -> HWND {
        list.handle.hwnd().unwrap() as _
    }

    fn top(list: &nwg::ListView) -> isize {
        unsafe { SendMessageW(hwnd(list), LVM_GETTOPINDEX, 0, 0) }
    }

    fn focused(list: &nwg::ListView) -> isize {
        unsafe { SendMessageW(hwnd(list), LVM_GETNEXTITEM, usize::MAX, LVNI_FOCUSED as isize) }
    }

    fn selected_count(list: &nwg::ListView) -> isize {
        unsafe { SendMessageW(hwnd(list), LVM_GETSELECTEDCOUNT, 0, 0) }
    }

    fn text(list: &nwg::ListView, row: usize, col: usize) -> String {
        list.item(row, col, 64).unwrap().text
    }

    #[test]
    fn scroll_and_keyboard_position_survive_an_update() {
        let f = fixture();
        sync(&f.list, &rows(100, "a"), Some(60));
        unsafe { SendMessageW(hwnd(&f.list), LVM_ENSUREVISIBLE, 70, 0) };
        let before = top(&f.list);
        assert!(before > 0, "the list must have scrolled");

        sync(&f.list, &rows(100, "b"), Some(60));

        assert_eq!(top(&f.list), before);
        assert_eq!(focused(&f.list), 60);
        assert_eq!(f.list.selected_item(), Some(60));
        assert_eq!(text(&f.list, 60, 0), "b60");
        assert_eq!(text(&f.list, 60, 1), "b60b");
    }

    #[test]
    fn keyboard_position_survives_with_nothing_selected() {
        let f = fixture();
        sync(&f.list, &rows(10, "a"), Some(4));
        sync(&f.list, &rows(10, "a"), None);
        sync(&f.list, &rows(10, "b"), None);
        assert_eq!(focused(&f.list), 4);
        assert_eq!(selected_count(&f.list), 0);
    }

    #[test]
    fn selection_follows_the_row_and_clears_when_it_is_gone() {
        let f = fixture();
        sync(&f.list, &rows(10, "a"), Some(3));
        sync(&f.list, &rows(10, "a"), Some(5));
        assert_eq!(f.list.selected_item(), Some(5));
        assert_eq!(selected_count(&f.list), 1);
        assert_eq!(focused(&f.list), 5);

        sync(&f.list, &rows(10, "a"), None);
        assert_eq!(selected_count(&f.list), 0);
    }

    #[test]
    fn rows_are_added_removed_and_updated() {
        let f = fixture();
        sync(&f.list, &rows(10, "a"), None);
        sync(&f.list, &rows(3, "b"), None);
        assert_eq!(f.list.len(), 3);
        assert_eq!(text(&f.list, 2, 0), "b2");

        let mut grown = rows(6, "c");
        grown[0].image = Some(1);
        sync(&f.list, &grown, None);
        assert_eq!(f.list.len(), 6);
        assert_eq!(text(&f.list, 5, 1), "c5b");
        assert_eq!(f.list.item(0, 0, 8).unwrap().image, 1);

        sync(&f.list, &[], None);
        assert_eq!(f.list.len(), 0);
    }
}
