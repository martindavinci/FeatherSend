# Manual test checklist

Run on a Windows 7 SP1 VM, the Windows 8.1 PC and a Windows 10/11 PC, with a
phone running the official LocalSend app on the same Wi-Fi.

## Start

- [ ] Packages contain `THIRD-PARTY-LICENSES.txt` (zip, and the install folder).
- [ ] Portable zip: unzip, run `FeatherSend.exe`; the firewall prompt appears; allow private networks.
- [ ] Network profile **Public** (Settings → Network, or Network and Sharing Center on Windows 7): the prompt must also be allowed for public networks. Allowed on private only: the phone neither appears nor sees the PC; allowed on public too, or profile switched to Private: both work without restarting.
- [ ] `data\` is created next to the exe (portable) / `%APPDATA%\FeatherSend` (installed).
- [ ] Installer: installs without UAC, Start menu entry, optional autostart, uninstall removes it.
- [ ] Second launch brings the running window to the front.
- [ ] Closing the window hides it to the tray; tray menu Quit exits.
- [ ] Idle memory under 30 MB (Task Manager, private working set; v0.1 measured 13.8 MB, expect about the same).

## Main window

Check on Windows 7 too: theme, icons and buttons draw through different code there.

- [ ] Empty lists: a gray centered hint in each; it goes away with the first row and stays centered while resizing.
- [ ] Status bar: `This PC: <name> · <address> · port 53317`. That address works in **Add by IP…** on the other device (also with Hyper-V or VirtualBox adapters present). **About** lists all addresses.
- [ ] No horizontal scroll bar in either list at any window size, including with so many rows that the vertical bar appears. Columns follow the window.
- [ ] Nothing selected: Send files, Send folder, Send clipboard, Pair, Cancel, Resume are disabled; Add by IP, Open received folder, Settings, About stay enabled.
- [ ] Online unpaired device selected: send and Pair enabled. Paired: Pair disabled. Offline: send and Pair disabled; the right-click menu grays the same items.
- [ ] Transfer selected. Running: Cancel only. Interrupted (paused): Cancel and Resume. Done, failed or cancelled: neither.
- [ ] Send files stands out: taller, bold, with the arrow. Groups are visibly apart; Open received folder, Settings and About sit at the bottom right. At the minimum window size nothing overlaps.
- [ ] Device icons: phone and computer are told apart at 100 %; green check on paired devices; gray when offline.
- [ ] Display at 150 % and 200 %: layout scales, text fits the buttons, icons stay crisp.
- [ ] Double-click: online device opens the file dialog for it; offline device shows the offline message; empty area does nothing.
- [ ] **Open received folder** (button and tray menu) opens the download folder in Explorer, creating it if missing, also after changing it in Settings.
- [ ] Download folder on a dead network share or removed drive: **Open received folder** leaves the window responsive and ends with an error message.
- [ ] Clicking a "Received …" balloon opens the same folder.
- [ ] Keyboard: Tab to **Cancel** on a running transfer and press Space: the arrow keys work on the transfers list afterwards. Same with **Pair** on the devices list.
- [ ] Drag files from Explorer onto a device row: they go to that device, even with another one selected, and the row gets selected. A folder keeps its structure.
- [ ] Drop on an offline row: the offline message, nothing sent.
- [ ] Drop outside the rows (transfer list, buttons, empty part of the device list): selected online device; with none selected and one online, that one; with several online, a menu at the cursor; with none online, the message.
- [ ] GDI objects (Task Manager column) stay flat, under about 200, while dragging the window edge for 30 s.
- [ ] Maximize, restore and Win+Up: lists, columns and buttons follow the window every time.
- [ ] During a transfer, with enough rows to scroll: the list stays where you scrolled it and the arrow keys continue from the selected row. The selected row stays selected, also when rows move. Removing the last row brings the empty hint back.
- [ ] Update notice, on a build older than the latest release: balloon and status bar text; clicking either opens the release page; **About** offers it too; no prompt at startup.

## Network changes

Keep the phone's LocalSend open on the same Wi-Fi.

- [ ] Sleep the PC for a few minutes, then wake it: the log shows `restarting discovery (resume from sleep)`; the phone is back in the list within about a minute, no restart needed; the phone finds the PC again.
- [ ] Turn Wi-Fi off: within about 15 s the status bar shows `no network` (or the remaining addresses) and the log shows `restarting discovery (…)`. Turn it on: the address returns and the phone is found again.
- [ ] New address (`ipconfig /release`, `/renew` with another lease, or another Wi-Fi): the status bar updates and discovery works on the new network.
- [ ] Short Wi-Fi drop with the same address: discovery still works both ways afterwards.
- [ ] Leave it running overnight with Wi-Fi off or the PC sleeping: `feathersend.log` holds a handful of multicast lines with `[N similar lines suppressed]`, not hundreds (v0.1 wrote 1023 in 3 hours).

## Log and crashes

- [ ] Phone → PC text message: one line `text message from <alias> (<n> characters)`, and the text appears nowhere in the log.
- [ ] Release build with `FEATHERSEND_PANIC_TEST=1` set: the error box names the log file, and it ends with `ERROR feathersend::panic: thread 'panic-test' panicked at …`. Exits after OK.

## LocalSend interop

- [ ] The phone appears in the device list within ~30 s (and FeatherSend appears on the phone).
- [ ] Phone → PC: a photo, then a folder; prompt shows sender, count and size; files land in `Downloads\FeatherSend` with folders kept.
- [ ] PC → phone: files and a folder.
- [ ] Phone → PC text message: the message prompt offers to copy it.
- [ ] "Send clipboard" with text to the phone arrives as a LocalSend message.
- [ ] Declining on the phone shows "declined" on the PC.
- [ ] Phone → PC, a big file, Cancel on the PC mid-way: the phone reports a failure at once, and no file stays in the download folder.
- [ ] More than 5 text messages in a minute from the phone: the next ones are refused, and no more boxes open.
- [ ] Leave a "files incoming" box open past 2 minutes, then answer Yes: a message says the request expired.

## FeatherSend ↔ FeatherSend

- [ ] Both PCs show each other as FeatherSend.
- [ ] Pair: both show the same code; confirm on both → Paired on both; cancel on one → nothing stored.
- [ ] Paired transfer of a large file (> 1 GB): unplug Wi-Fi mid-way, reconnect → resumes (or Resume button), final file hash matches.
- [ ] Clipboard hotkey (Ctrl+Alt+C): text, a screenshot (image), copied files (arrive as files and are on the receiver's clipboard).
- [ ] Auto-sync on: copying text on one PC appears on the other without prompts; no echo loop.
- [ ] Auto-accept files on: no prompt for that device only.
- [ ] Unpair on one side: the other side's clipboard pushes get refused.
- [ ] Auto-accept off: receive a file, Cancel it mid-way on the receiver, send it again: the receiver asks again. The same after a completed file, once 10 minutes have passed.

## Settings

- [ ] Change download folder, hotkey, start with Windows; restart keeps them.
- [ ] Esc closes Settings without saving, from any field. With the language list open, Esc only closes the list.
- [ ] Language: Automatic follows the Windows display language; each of the 9 languages in Settings applies after restart, including Japanese, Korean and Chinese on a PC with no East Asian language pack (Windows 7 too: text must not show as boxes).
- [ ] Installer: offers the 9 languages and picks the Windows one.
- [ ] "Let unpaired LocalSend devices ask" off: the phone is refused without a prompt.
