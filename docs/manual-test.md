# Manual test checklist

Run on a Windows 7 SP1 VM, the Windows 8.1 PC and a Windows 10/11 PC, with a
phone running the official LocalSend app on the same Wi-Fi.

## Start

- [ ] Portable zip: unzip, run `FeatherSend.exe`; the firewall prompt appears; allow private networks.
- [ ] `data\` is created next to the exe (portable) / `%APPDATA%\FeatherSend` (installed).
- [ ] Installer: installs without UAC, Start menu entry, optional autostart, uninstall removes it.
- [ ] Second launch brings the running window to the front.
- [ ] Closing the window hides it to the tray; tray menu Quit exits.
- [ ] Idle memory under 30 MB (Task Manager, private working set).

## LocalSend interop

- [ ] The phone appears in the device list within ~30 s (and FeatherSend appears on the phone).
- [ ] Phone → PC: a photo, then a folder; prompt shows sender, count and size; files land in `Downloads\FeatherSend` with folders kept.
- [ ] PC → phone: files and a folder.
- [ ] Phone → PC text message: the message prompt offers to copy it.
- [ ] "Send clipboard" with text to the phone arrives as a LocalSend message.
- [ ] Declining on the phone shows "declined" on the PC.

## FeatherSend ↔ FeatherSend

- [ ] Both PCs show each other as FeatherSend.
- [ ] Pair: both show the same code; confirm on both → Paired on both; cancel on one → nothing stored.
- [ ] Paired transfer of a large file (> 1 GB): unplug Wi-Fi mid-way, reconnect → resumes (or Resume button), final file hash matches.
- [ ] Clipboard hotkey (Ctrl+Alt+C): text, a screenshot (image), copied files (arrive as files and are on the receiver's clipboard).
- [ ] Auto-sync on: copying text on one PC appears on the other without prompts; no echo loop.
- [ ] Auto-accept files on: no prompt for that device only.
- [ ] Unpair on one side: the other side's clipboard pushes get refused.

## Settings

- [ ] Change download folder, hotkey, start with Windows; restart keeps them.
- [ ] Language Italian/English after restart.
- [ ] "Let unpaired LocalSend devices ask" off: the phone is refused without a prompt.
