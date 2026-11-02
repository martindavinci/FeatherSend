<p align="center">
  <img src="docs/img/icon.png" width="128" height="128" alt="FeatherSend">
</p>

<h1 align="center">FeatherSend</h1>

<p align="center">Share files, folders and the clipboard between devices on your local network. Light enough for old PCs.</p>

<p align="center">
  <a href="https://github.com/martindavinci/FeatherSend/actions/workflows/ci.yml"><img src="https://github.com/martindavinci/FeatherSend/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/martindavinci/FeatherSend/releases/latest"><img src="https://img.shields.io/github/v/release/martindavinci/FeatherSend" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/martindavinci/FeatherSend" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-Windows%207%20%7C%208.1%20%7C%2010%20%7C%2011-0078D6" alt="Platform: Windows 7 | 8.1 | 10 | 11">
</p>

## Why

Most file-sharing apps for the local network need Windows 10 or later. FeatherSend runs on
**Windows 7 SP1 and 8.1** as well, on 32-bit and 64-bit PCs with little memory (it idles at
a few MB), with no runtime to install and no GPU needed.

It speaks the [LocalSend](https://localsend.org) protocol, so phones and other computers just
use the official LocalSend app: no FeatherSend app needed there.

## Features

- **Files and folders** to any FeatherSend or LocalSend device on the same network. Folders keep
  their structure. Drop files on a device in the list to send them there.
- **Pairing** between FeatherSend devices: both screens show the same 6-digit code and both
  users confirm it. Paired devices are recognised by their certificate, never by name or address.
- **Clipboard**: text, images and copied files. Send it with **Ctrl+Alt+C** (configurable) or the
  tray menu; turn on automatic sync per paired device. Images up to 16 MB travel as images, larger
  ones and copied files travel as a normal transfer.
- **Resume**: a transfer between FeatherSend devices that drops (Wi-Fi hiccup, sleep) continues
  from where it stopped, automatically or with **Resume**.
- **You decide**: every incoming transfer and clipboard asks first, unless you turn on automatic
  accept for a paired device. Unpaired LocalSend devices can be refused entirely in **Settings**.
- Tray icon, optional start with Windows, installer or portable.
- English, Spanish, French, German, Italian, Brazilian Portuguese, Japanese, Korean and
  Simplified Chinese: follows the Windows language, or choose one in Settings.

## Download

From [Releases](https://github.com/martindavinci/FeatherSend/releases):

- `FeatherSend-<version>-x64-setup.exe` / `-x86-setup.exe`: installer, no administrator rights
  needed.
- `FeatherSend-<version>-x64-portable.zip` / `-x86-portable.zip`: unzip and run; settings stay
  in the `data` folder next to the exe.

Windows Firewall asks the first time FeatherSend starts: allow it on private networks, and on public
ones too if your Wi-Fi is set as public (Windows often does that). If devices do not see each other,
check that network profile and that the router does not isolate Wi-Fi clients (AP isolation); you can
also add a device with **Add by IP…**, using the address shown at the bottom of the other device's
window.

## Privacy

Everything stays on your network. The only internet request is a daily check of the latest
release on GitHub (**Check for updates** in **Settings**).

## Building

Requires the pinned nightly in `rust-toolchain.toml` (with `rust-src`) and Visual Studio 2022 with
the C++ tools (VS 2026 libraries no longer target Windows 7/8.1).

```
cargo test --workspace                 # tests on the host
cargo xtask build-win7 --arch all      # x64 + x86 builds for Windows 7+
cargo xtask check-imports <exe>        # no API missing on Windows 7
cargo xtask dist                       # zips, installers, SHA256SUMS (needs Inno Setup 6)
```

## License

MIT, see [LICENSE](LICENSE). FeatherSend includes the LocalSend core (Apache-2.0), see
[NOTICE](NOTICE). The other libraries and their licenses are in
[THIRD-PARTY-LICENSES.txt](packaging/THIRD-PARTY-LICENSES.txt).
