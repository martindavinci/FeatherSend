# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-11-02

First release.

### Added

- Send and receive files and folders with FeatherSend and LocalSend devices (LocalSend protocol v2).
- Pairing between FeatherSend devices with a 6-digit code confirmed on both screens.
- Clipboard sharing (text, images, files) with a global hotkey and optional auto-sync per device.
- Resumable transfers between FeatherSend devices. Cancelling one stops sending immediately and tells the receiver.
- Approval for every incoming transfer, with optional auto-accept per paired device.
- Discovery that recovers by itself after sleep, a Wi-Fi reconnect or a new IP address.
- Drop files on a device in the list to send them to that device.
- This PC's name, address and port in a status bar.
- Device icons (phone or computer, paired, offline), hints in empty lists and "Open received folder".
- Buttons enabled only when they apply to the selected device or transfer.
- Tray icon, start with Windows, Italian and English interface.
- Update check, with a balloon and the status bar when a new version is out.
- A log that writes repeated warnings once every 10 minutes with a count, and records crashes with a message saying where to find them.
- Windows 7 SP1+ builds for x64 and x86, installer and portable zip.

[0.1.0]: https://github.com/martindavinci/FeatherSend/releases/tag/v0.1.0
