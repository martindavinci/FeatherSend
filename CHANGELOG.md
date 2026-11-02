# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- Send and receive files and folders with FeatherSend and LocalSend devices (LocalSend protocol v2).
- Pairing between FeatherSend devices with a 6-digit code confirmed on both screens.
- Clipboard sharing (text, images, files) with a global hotkey and optional auto-sync per device.
- Resumable transfers between FeatherSend devices.
- Approval for every incoming transfer, with optional auto-accept per paired device.
- Tray icon, start with Windows, Italian and English, update check.
- Windows 7 SP1+ builds for x64 and x86, installer and portable zip.
- This PC's name, address and port in a status bar, which also shows when an update is available.
- Device icons (phone or computer, paired, offline), hints in empty lists and "Open received folder".
- Drop files on a device in the list to send them to that device.

### Changed

- Buttons are enabled only when they apply to the selected device or transfer; "Send files" stands out.
- Update notice: a balloon and the status bar instead of a prompt at startup. Clicking a balloon opens the download page or the received folder.

### Fixed

- Discovery recovers by itself after sleep, a Wi-Fi reconnect or a new IP address.
- A failing multicast send no longer fills the log: repeated warnings and errors are written once every 10 minutes with a count.
- Panics are written to the log before the program closes, and a message says where to find them.
- Cancelling a transfer between FeatherSend devices stops sending immediately, and the receiver always hears about it.
- List columns fit the window, without a horizontal scroll bar.
- Resizing the window no longer leaks a Windows GDI handle per control, and dropped files no longer leak memory.
