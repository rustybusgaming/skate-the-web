//! Typed view of the JavaScript host object the page provides.
//!
//! `skateHost.getBootConfig(): Promise<{root, mount, map, difficulty}>`
//! `skateHost.readFile(path): Promise<Uint8Array | null>`
//! `skateHost.writeFile(path, Uint8Array | null)` (null deletes)
//! `skateHost.progress(done, total, label)`
//! `skateHost.stage(label)`
//! `skateHost.ready()`
//! `skateHost.takeMusicCommand() / musicStatus(title, paused, available)`
//! `skateHost.fatal(message)`
//! `skateHost.netOpen() / netClose() / netSend(frame) / netText(text) / netPoll() / netBacklog()`
//!   (multiplayer relay socket, see web/net.js)
use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = skateHost, js_name = getBootConfig, catch)]
    async fn get_boot_config() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = skateHost, js_name = readFile, catch)]
    async fn read_file(path: &str) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = skateHost, js_name = writeFile)]
    fn write_file(path: &str, data: Option<Uint8Array>);
    #[wasm_bindgen(js_namespace = skateHost, js_name = progress)]
    pub fn progress(done: u32, total: u32, label: &str);
    #[wasm_bindgen(js_namespace = skateHost, js_name = stage)]
    pub fn stage(label: &str);
    #[wasm_bindgen(js_namespace = skateHost, js_name = ready)]
    pub fn ready();
    /// The page may drop its copy of the startup files once they are mounted.
    #[wasm_bindgen(js_namespace = skateHost, js_name = releaseBoot)]
    pub fn release_boot();
    /// Trackpad Mode level (0 = off) and whether gameplay has the input.
    /// Open a link in a new tab.
    /// Download a file into the page's saved downloads ahead of time.
    #[wasm_bindgen(js_namespace = skateHost, js_name = prefetch)]
    pub fn prefetch(path: &str, background: bool);
    /// [bytes so far, total, ms since bytes last came] of a file downloading.
    #[wasm_bindgen(js_namespace = skateHost, js_name = downloadStatus)]
    fn download_status_js(path: &str) -> JsValue;
    #[wasm_bindgen(js_namespace = skateHost, js_name = openUrl)]
    pub fn open_url(url: &str);
    /// Ask the player for a file (`accept`: a file type list, e.g. ".cfss").
    #[wasm_bindgen(js_namespace = skateHost, js_name = pickFile)]
    pub fn pick_file(accept: &str);
    /// The bytes of a file the player chose or dropped, once.
    #[wasm_bindgen(js_namespace = skateHost, js_name = takePicked)]
    fn take_picked_js() -> JsValue;
    /// The chat box has the keyboard: the page's keyboard controller stops.
    #[wasm_bindgen(js_namespace = skateHost, js_name = setTyping)]
    pub fn set_typing(typing: bool);
    /// The display's refresh rate as the page measured it (0 until known).
    #[wasm_bindgen(js_namespace = skateHost, js_name = displayHz)]
    pub fn display_hz() -> f64;
    /// Mobile mode switched in the menu: the page's memory savers follow it.
    #[wasm_bindgen(js_namespace = skateHost, js_name = setMobile)]
    pub fn set_mobile(on: bool);
    #[wasm_bindgen(js_namespace = skateHost, js_name = setTrackpad)]
    pub fn set_trackpad(level: u32, playing: bool);
    #[wasm_bindgen(js_namespace = skateHost, js_name = takeMusicCommand)]
    pub fn take_music_command() -> String;
    #[wasm_bindgen(js_namespace = skateHost, js_name = musicStatus)]
    pub fn music_status(title: &str, paused: bool, available: bool);
    #[wasm_bindgen(js_namespace = skateHost, js_name = fatal)]
    fn js_fatal(message: &str);
    #[wasm_bindgen(js_namespace = skateHost, js_name = netOpen)]
    pub fn net_open();
    #[wasm_bindgen(js_namespace = skateHost, js_name = netClose)]
    pub fn net_close();
    #[wasm_bindgen(js_namespace = skateHost, js_name = netSend)]
    pub fn net_send(frame: &[u8]) -> bool;
    #[wasm_bindgen(js_namespace = skateHost, js_name = netText)]
    pub fn net_text(message: &str) -> bool;
    #[wasm_bindgen(js_namespace = skateHost, js_name = netPoll)]
    fn net_poll() -> Array;
    #[wasm_bindgen(js_namespace = skateHost, js_name = netBacklog)]
    pub fn net_backlog() -> f64;
}

/// One message from the multiplayer relay socket.
pub(crate) enum NetMessage {
    Text(String),
    Datagram(Vec<u8>),
}

pub(crate) fn net_messages() -> Vec<NetMessage> {
    net_poll()
        .iter()
        .map(|value| match value.as_string() {
            Some(text) => NetMessage::Text(text),
            None => NetMessage::Datagram(Uint8Array::new(&value).to_vec()),
        })
        .collect()
}

pub(super) struct BootConfig {
    pub root: String,
    pub mount: Vec<String>,
    pub map: Option<String>,
    pub difficulty: Option<String>,
    pub capture: bool,
    pub teleport: Option<String>,
    pub front_end: bool,
    pub mobile: bool,
    pub declared: Vec<String>,
}

fn text(object: &JsValue, key: &str) -> Option<String> {
    Reflect::get(object, &JsValue::from_str(key)).ok()?.as_string()
}

pub(super) async fn boot_config() -> Result<BootConfig, String> {
    let object = get_boot_config().await.map_err(|e| format!("getBootConfig failed: {e:?}"))?;
    let mount = Reflect::get(&object, &JsValue::from_str("mount"))
        .map_err(|e| format!("boot config: {e:?}"))?;
    Ok(BootConfig {
        root: text(&object, "root").unwrap_or_else(|| "assets".into()),
        mount: Array::from(&mount).iter().filter_map(|v| v.as_string()).collect(),
        map: text(&object, "map"),
        difficulty: text(&object, "difficulty"),
        capture: Reflect::get(&object, &JsValue::from_str("capture")).ok().and_then(|v| v.as_bool()).unwrap_or(false),
        teleport: text(&object, "teleport"),
        front_end: Reflect::get(&object, &JsValue::from_str("frontEnd")).ok().and_then(|v| v.as_bool()).unwrap_or(false),
        mobile: Reflect::get(&object, &JsValue::from_str("mobile")).ok().and_then(|v| v.as_bool()).unwrap_or(false),
        declared: Reflect::get(&object, &JsValue::from_str("declared"))
            .ok()
            .filter(|v| v.is_object())
            .map(|v| Array::from(&v).iter().filter_map(|v| v.as_string()).collect())
            .unwrap_or_default(),
    })
}

/// `Ok(None)` means the host has no such file.
pub(super) async fn read(path: &str) -> Result<Option<Vec<u8>>, String> {
    let value = read_file(path).await.map_err(|e| format!("{path}: {e:?}"))?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    Ok(Some(Uint8Array::new(&value).to_vec()))
}

pub(super) fn persist(path: &str, data: Option<&[u8]>) {
    write_file(path, data.map(Uint8Array::from));
}

pub(super) fn fatal(message: &str) {
    js_fatal(message);
}

/// How far a file's download has got: (bytes, total, seconds since bytes last
/// arrived). None when it is not downloading.
pub fn download_status(path: &str) -> Option<(f64, f64, f64)> {
    let value = download_status_js(path);
    let array = value.dyn_ref::<js_sys::Array>()?;
    let at = |i: u32| array.get(i).as_f64().unwrap_or(0.);
    Some((at(0), at(1), at(2) / 1000.))
}

/// The file the player chose or dropped on the page, if one is waiting.
pub fn take_picked() -> Option<Vec<u8>> {
    let value = take_picked_js();
    value.dyn_ref::<Uint8Array>().map(Uint8Array::to_vec)
}
