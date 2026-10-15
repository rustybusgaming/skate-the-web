//! Browser entry. The page defines `globalThis.skateHost` (see `host.rs`),
//! then loads the wasm module; `main` pulls its boot configuration and files
//! from that object, mounts them into `skate_data::vfs` and starts the app.
pub(crate) mod asset_reader;
mod host;
mod log;
mod start;

/// Tell the page's loading screen which stage of startup is running. The parts
/// of the engine that are not web-only call this through `platform`.
pub(crate) fn stage(label: &str) {
    host::stage(label);
}

/// Have the page download a file ahead of time (`background`: after anything
/// asked for now).
pub(crate) fn prefetch(path: &str, background: bool) {
    host::prefetch(path, background);
}

/// A file from the page's game data; `Ok(None)` when it has no such file.
pub(crate) async fn host_read(path: &str) -> Result<Option<Vec<u8>>, String> {
    host::read(path).await
}

/// The multiplayer relay socket the page keeps for the engine (web/net.js).
/// Open a link in a new browser tab.
pub(crate) fn open_url(url: &str) {
    host::open_url(url);
}

/// Ask the player for a file; it arrives through `take_picked`.
pub(crate) fn pick_file(accept: &str) {
    host::pick_file(accept);
}

/// A file the player chose or dropped on the page (never uploaded).
pub(crate) fn take_picked() -> Option<Vec<u8>> {
    host::take_picked()
}

/// Trackpad Mode: the page drives the right stick from the pointer.
pub(crate) fn set_trackpad(level: u8, playing: bool) {
    host::set_trackpad(u32::from(level), playing);
}

/// Take one queued music-player action from the page.
pub(crate) fn take_music_command() -> String {
    host::take_music_command()
}

/// Keep the page's music controls in sync with the active soundtrack.
pub(crate) fn music_status(title: &str, paused: bool, available: bool) {
    host::music_status(title, paused, available);
}

/// A file's download progress: (bytes, total, seconds since bytes arrived).
pub(crate) fn download_status(path: &str) -> Option<(f64, f64, f64)> {
    host::download_status(path)
}

/// The display's refresh rate, 0 until the page has measured it.
pub(crate) fn display_hz() -> f64 {
    host::display_hz()
}

/// With ?trace-reads, each file the engine opens is logged once.
pub(crate) fn log_first_reads() {
    for path in skate_data::vfs::first_reads() {
        bevy::log::info!("VFS_READ {path}");
    }
}

/// Mobile mode changed in the menu (the page stops saving big downloads and
/// fetching worlds ahead of time).
pub(crate) fn set_mobile(on: bool) {
    host::set_mobile(on);
}

/// Chat typing: keys are text, not the keyboard controller.
pub(crate) fn set_typing(typing: bool) {
    host::set_typing(typing);
}

pub(crate) mod net {
    pub(crate) use super::host::{
        NetMessage as Message, net_backlog as backlog, net_close as close, net_messages as poll,
        net_open as open, net_send as send, net_text as text,
    };
}

pub(crate) fn main() {
    console_error_panic_hook::set_once();
    log::install();
    wasm_bindgen_futures::spawn_local(async {
        if let Err(error) = start::boot().await {
            host::fatal(&error);
        }
    });
}
