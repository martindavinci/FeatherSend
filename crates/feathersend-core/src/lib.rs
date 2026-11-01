//! FeatherSend's platform-neutral core: settings, identity, trust, pairing,
//! clipboard model, resumable transfers and the network backend that speaks
//! LocalSend v2 (via LocalSend's own core crate) plus FeatherSend's extension
//! protocol. The Win32 GUI lives in the `app` crate.

pub mod backend;
pub mod clip;
pub mod ext;
pub mod i18n;
pub mod identity;
pub mod pairing;
pub mod paths;
pub mod resume;
pub mod settings;
pub mod trust;
pub mod update;
pub mod util;
