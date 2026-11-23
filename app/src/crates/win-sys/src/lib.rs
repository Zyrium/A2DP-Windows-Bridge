//! Win32 APIs for BlueCaster that no Rust library covers. The only crate besides
//! the `*-sys` crates where `unsafe` is allowed; every function here is
//! safe to call.

pub mod accent;
pub mod acl;
pub mod app_icon;
pub mod audio_notify;
pub mod audio_policy;
pub mod authenticode;
pub mod autostart;
pub mod bt_keys;
pub mod bt_paired;
pub mod bt_radios;
pub mod com;
pub mod computer;
pub mod console;
pub mod crash;
pub mod dpapi;
pub mod driver;
pub mod elevation;
pub mod file_dialog;
pub mod hid_guard;
pub mod input_desktop;
pub mod input_inject;
pub mod locale;
pub mod media_keys;
pub mod mmcss;
pub mod notify;
pub mod perf;
pub mod pipe;
pub mod pipe_peer;
pub mod power;
pub mod process;
pub mod quick_settings;
pub mod service;
pub mod service_files;
pub mod service_query;
pub mod session_launch;
pub mod shm;
pub mod single_instance;
pub mod usb_adapters;
pub mod usb_control;
pub mod usb_isoch;
pub mod webview;
pub mod wlan;
