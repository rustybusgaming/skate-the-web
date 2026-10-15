//! Native-resolution pause UI over a separately scaled 3D render target.
use crate::difficulty::Difficulty;
use crate::custom_difficulty::{Tuning, OPTIONS};
use bevy::ui::RelativeCursorPosition;
use bevy::{
    camera::RenderTarget,
    image::ImageSampler,
    prelude::*,
    render::{
        render_resource::{Extent3d, TextureFormat},
    },
    window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode},
};
use serde::{Deserialize, Serialize};
use web_time::{Instant};
use std::{
    path::PathBuf,
    time::Duration,
};

/// (0, 0) is Auto: the window keeps whatever size the desktop or page gives it.
const RESOLUTIONS: &[(u32, u32)] = &[
    (0, 0),
    (1280, 720),
    (1280, 800),
    (1600, 900),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
    (2560, 1600),
    (3840, 2160),
];
const SCALES: &[u32] = &[25, 50, 67, 75, 85, 100];
const DAY_SPEEDS: &[u32] = &[0, 1, 10, 30, 60, 120, 360, 720];
/// Frame rate caps, in menu order; 0 is Unlimited.
const LIMITS: &[u32] = &[30, 60, 75, 100, 120, 144, 165, 240, 360, 0];
const BRIGHTNESS: &[u32] = &[60, 70, 80, 90, 100, 110, 120, 130, 140, 150];
const SHADOW_SIZES: [usize; 4] = [1024, 1024, 2048, 4096];

/// Map texture quality for the next world build (see retail_render). Set from
/// the saved settings before the first map is built, then on every change.
pub(crate) static TEXTURE_QUALITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// A graphics option the CrossBar sets directly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum GraphicsField {
    TextureQuality,
    Shadows,
    AntiAliasing,
    Brightness,
    /// Picker index 0 is VSync; the rest are the frame limits.
    FrameLimit,
    ShowHud,
    ShowFps,
    /// Off / On: Lite graphics plus the memory savers (see `mobile`).
    MobileMode,
    PerformancePreset,
}

/// Mobile mode's graphics: what a phone's browser can hold.
const MOBILE_PRESET: (u8, u8, u8, u32) = (3, 0, 0, 75);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct GraphicsSettings {
    width: u32,
    height: u32,
    /// Borderless fullscreen at the display's own resolution.
    fullscreen: bool,
    /// Map texture size: 0 full (as shipped), 1 half, 2 quarter. Applies when
    /// a map is built.
    texture_quality: u8,
    /// 0 off, 1 low (1024), 2 medium (2048, Bevy's and the default), 3 high (4096).
    shadows: u8,
    /// 0 off, 1 FXAA, 2 SMAA.
    anti_aliasing: u8,
    /// Multiplier on the adapted exposure, in percent.
    brightness: u32,
    vsync: bool,
    show_hud: bool,
    show_fps: bool,
    /// Mobile mode: lighter graphics and memory savers for phone browsers.
    mobile: bool,
    scale: u32,
    fps: u32,
    hour: f32,
    day_speed: u32,
    ambient_level: Option<u32>,
}
impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            fullscreen: true,
            // Phones and tablets start at Low: a city at full texture size
            // needs more memory than their browsers allow a tab, and iOS
            // closes the tab outright. Saved settings still win.
            texture_quality: if crate::platform::small_device() { 2 } else { 0 },
            shadows: 2,
            anti_aliasing: 0,
            brightness: 100,
            vsync: false,
            show_hud: true,
            show_fps: true,
            mobile: false,
            scale: 100,
            fps: 0,
            hour: 12.,
            day_speed: 60,
            ambient_level: None,
        }
    }
}
impl GraphicsSettings {
    // Browser defaults apply only when this browser has no saved graphics file.
    fn for_browser(browser: crate::config::BrowserProfile) -> Self {
        let mut settings = Self::default();
        match browser {
            crate::config::BrowserProfile::Firefox => {
                settings.texture_quality = 1;
                settings.shadows = 1;
                settings.scale = 85;
                settings.fps = 60;
            }
            crate::config::BrowserProfile::Safari => {
                settings.texture_quality = 2;
                settings.shadows = 0;
                settings.scale = 75;
                settings.fps = 60;
            }
            crate::config::BrowserProfile::Edge
            | crate::config::BrowserProfile::Chromium
            | crate::config::BrowserProfile::Other => {}
        }
        settings
    }

    fn load(path: &std::path::Path, browser: crate::config::BrowserProfile) -> Self {
        match skate_data::vfs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes).unwrap_or_else(|error| {
                warn!("Graphics settings: {error}");
                Self::for_browser(browser)
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::for_browser(browser),
            Err(error) => {
                warn!("Could not read graphics settings {}: {error}", path.display());
                Self::for_browser(browser)
            }
        }
    }
}
impl GraphicsSettings {
    fn performance_preset(&self) -> usize {
        if self.texture_quality == 3
            && self.shadows == 0
            && self.anti_aliasing == 0
            && self.scale == 50
            && self.fps == 60
            && !self.vsync
        {
            2
        } else if self.texture_quality == 0
            && self.shadows == 2
            && self.anti_aliasing == 0
            && self.scale == 100
            && self.fps == 0
            && !self.vsync
        {
            1
        } else {
            0
        }
    }

    fn validated(mut self) -> Self {
        self.ambient_level = self.ambient_level.map(|level| level.min(100));
        self.hour = if self.hour.is_finite() { self.hour.rem_euclid(24.) } else { 12. };
        if !DAY_SPEEDS.contains(&self.day_speed) { self.day_speed = 60; }
        // The page sizes the canvas in a browser; a fixed size would only
        // stretch it.
        if !RESOLUTIONS.contains(&(self.width, self.height)) || cfg!(target_arch = "wasm32") {
            (self.width, self.height) = (0, 0);
        }
        if !SCALES.contains(&self.scale) {
            self.scale = 100;
        }
        if !LIMITS.contains(&self.fps) {
            // A cap from an older list (90) becomes the nearest one offered;
            // anything else (a damaged file) is Unlimited.
            let wanted = self.fps;
            self.fps = LIMITS.iter().copied().filter(|l| *l > 0 && l.abs_diff(wanted) <= 15).min_by_key(|l| l.abs_diff(wanted)).unwrap_or(0);
        }
        // 3 is Lowest (one eighth size), Lite mode's level for phones.
        self.texture_quality = self.texture_quality.min(3);
        self.shadows = self.shadows.min(3);
        self.anti_aliasing = self.anti_aliasing.min(2);
        if !BRIGHTNESS.contains(&self.brightness) {
            self.brightness = 100;
        }
        self
    }
    fn auto_size(&self) -> bool {
        self.width == 0 || self.height == 0
    }
    fn internal_size(&self, window: UVec2) -> UVec2 {
        (window * self.scale / 100).max(UVec2::ONE)
    }
}
/// A way online held back until the player has a name: an Online-page row
/// (host, join by code), or a lobby browser row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Online {
    Row(usize),
    Browser(usize),
    /// Just choosing a name (Player Name in the pause menu).
    Rename,
}

/// While the name prompt is up (1), or after it was left without a name (2):
/// a game mode waiting for its hosted session holds, then is dropped.
static NAME_PROMPT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
pub(crate) fn name_prompt() -> u8 {
    NAME_PROMPT.load(std::sync::atomic::Ordering::Relaxed)
}
pub(crate) fn clear_name_prompt() {
    NAME_PROMPT.store(0, std::sync::atomic::Ordering::Relaxed);
}
/// The name was not chosen: what waited on it is dropped.
pub(crate) fn cancel_name_prompt() {
    let waiting = NAME_PROMPT.load(std::sync::atomic::Ordering::Relaxed) == 1;
    NAME_PROMPT.store(if waiting { 2 } else { 0 }, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Resource)]
pub(crate) struct Menu {
    pub(crate) open: bool,
    selected: usize,
    settings: GraphicsSettings,
    path: PathBuf,
    difficulty: Difficulty,
    custom: Tuning,
    custom_defaults: Tuning,
    custom_apply: bool,
    custom_dirty: bool,
    status: String,
    maps: Vec<crate::map_library::Entry>,
    selected_map: usize,
    multiplayer: bool,
    browser: bool,
    network_page: u8,
    browser_count: usize,
    daylight: bool,
    section: usize,
    custom_sections: Vec<(String, Vec<(String,String,String)>)>,
    map_detail: bool,
    destinations: Vec<crate::teleport_menu::Destination>,
    pending_travel: Option<(Option<PathBuf>, [[f32; 4]; 4])>,
    /// Skate 3's original CrossBar is presenting the pause menu. The panel
    /// below stays hidden and ignores navigation; it shows only for the pages
    /// the CrossBar hands back to it (text entry, lobby browser, sliders).
    pub(crate) crossbar: bool,
    /// The controls card is over the menu (controls_help.rs).
    pub(crate) controls_help: bool,
    /// Skate 3's Trick Guide is open over the menu (trick_guide.rs).
    pub(crate) trick_guide: bool,
    /// Online: the game mode whose spot is being chosen (the second step of
    /// hosting one), and whether a session is to be hosted for it.
    pub(crate) mode_pick: Option<(crate::game_modes::Kind, bool)>,
    /// Going online with the default name: what to do once a name is chosen.
    pub(crate) pending_online: Option<Online>,
    /// Actions chosen in the CrossBar, run through the same code as the
    /// panel's own rows so settings, travel and saving behave identically.
    pub(crate) injected: Vec<(Page, usize, i32)>,
    /// The CrossBar is the main menu (main_menu.rs): it shows the front end's
    /// columns, and the front end owns Escape/Start.
    pub(crate) front_end: bool,
    /// The world the main menu's World picker points at (index into `maps`).
    pub(crate) front_world: usize,
    /// Skate was chosen on the main menu.
    pub(crate) launch: bool,
    /// The front end is showing its own screens; the panel stays hidden.
    pub(crate) hidden: bool,
}

/// Which panel page an injected row belongs to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Page {
    Section(usize),
    Multiplayer(u8),
    Map(usize),
    Daylight,
}

/// What a CrossBar row does when chosen.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CrossBarAction {
    /// Run a panel row.
    Row(Page, usize),
    /// Show the panel itself at this page; Back returns to the CrossBar.
    Panel(Page, usize),
    /// A picker over `values`; choosing steps the panel row by the difference.
    Pick { page: Page, row: usize, current: usize },
    /// Travel to the chosen map.
    Travel,
    /// Teleport to the chosen point of interest on this map.
    Poi,
    /// Toggle the free-flying debug camera.
    DebugCam,
    /// Drop the skater where the debug camera is.
    CameraTeleport,
    /// Show the controls card (keyboard, trackpad and controller layout).
    Controls,
    /// Open Skate 3's Trick Guide.
    TrickGuide,
    /// Open the Object Dropper (object_dropper.rs).
    ObjectDropper,
    /// Host: how many props each player may have at once (0..=50).
    PropLimit,
    /// Open the community Discord invite.
    Discord,
    /// Open the donation page.
    Donate,
    /// Open the Buy Me a Coffee page.
    Coffee,
    /// Offline: host a session for free skating (0) or for a game mode.
    HostMode,
    /// Hosting: pick a game mode to start (0 is the prompt).
    GameMode,
    /// The second step: start this game mode at the chosen spot (0: random).
    Mode(crate::game_modes::Kind),
    /// The second step's way back to the column.
    ModeBack,
    /// End the game mode that is running.
    EndMode,
    /// Host: kick the picked player. Others: call a vote to kick them.
    Kick,
    /// The session's players and their maps (a list to look at).
    Players,
    /// Choose the name other skaters see (Skate 3's popup).
    Rename,
    /// How many players a hosted session holds (value + 2).
    MaxPlayers,
    /// Vote yes (0) or no (1) in the open vote to kick.
    KickBallot,
    Resume,
    /// Main menu: drop in at the chosen world.
    Skate,
    /// Main menu: choose the world to skate.
    World,
    /// A player or camera preference (settings/player.json).
    Player(PlayerField),
    /// A graphics setting (settings/graphics.json).
    Graphics(GraphicsField),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PlayerField {
    Camera,
    FieldOfView,
    Shake,
    /// Off, horizontal, vertical, both.
    InvertLook,
    /// Off, 20% ... 100% (the original picker shows eight values at most).
    Volume,
    /// Off, left foot, right foot.
    AutoPush,
    /// Off, low, medium, high (browser).
    Trackpad,
}
impl PlayerField {
    /// Set the field from a picker index.
    pub(crate) fn set(self, settings: &mut crate::player_settings::PlayerSettings, value: usize) {
        let on = value != 0;
        match self {
            Self::Camera => settings.low_camera = on,
            Self::FieldOfView => {
                settings.fov_percent = crate::player_settings::FOV_STEPS.get(value).copied().unwrap_or(100)
            }
            Self::Shake => settings.camera_shake = on,
            Self::InvertLook => {
                settings.invert_x = value == 1 || value == 3;
                settings.invert_y = value == 2 || value == 3;
            }
            Self::Volume => settings.sound_volume = (value.min(5) * 20) as u32,
            Self::AutoPush => {
                settings.auto_push = value != 0;
                settings.push_right = value == 2;
            }
            Self::Trackpad => settings.trackpad = value.min(3) as u8,
        }
    }
}
impl Menu {
    #[cfg(test)]
    pub(crate) fn advance_day(&mut self, seconds: f32) -> f32 {
        if !self.open && self.settings.day_speed > 0 {
            self.settings.hour = (self.settings.hour + seconds * self.settings.day_speed as f32 / 3600.).rem_euclid(24.);
        }
        self.settings.hour
    }
    pub(crate) fn shadow_quality(&self) -> u8 {
        self.settings.shadows
    }
    pub(crate) fn anti_aliasing(&self) -> u8 {
        self.settings.anti_aliasing
    }
    pub(crate) fn brightness(&self) -> f32 {
        self.settings.brightness as f32 / 100.0
    }
    pub(crate) fn show_hud(&self) -> bool {
        self.settings.show_hud
    }
    pub(crate) fn show_fps(&self) -> bool {
        self.settings.show_fps
    }
    /// Apply a CrossBar choice and save it with the other graphics settings.
    /// Mobile mode is on: the big cities ask before loading.
    pub(crate) fn mobile(&self) -> bool {
        self.settings.mobile
    }
    pub(crate) fn set_graphics(&mut self, field: GraphicsField, value: usize) {
        let s = &mut self.settings;
        match field {
            GraphicsField::TextureQuality => {
                s.texture_quality = value.min(3) as u8;
                TEXTURE_QUALITY.store(s.texture_quality, std::sync::atomic::Ordering::Relaxed);
                self.status = "Texture quality applies the next time a map loads.".into();
            }
            GraphicsField::Shadows => s.shadows = value.min(3) as u8,
            GraphicsField::AntiAliasing => s.anti_aliasing = value.min(2) as u8,
            GraphicsField::Brightness => s.brightness = BRIGHTNESS.get(value).copied().unwrap_or(100),
            // The browser's list has no VSync row (it always paces to the display).
            GraphicsField::FrameLimit if cfg!(target_arch = "wasm32") => {
                s.vsync = false;
                s.fps = LIMITS.get(value).copied().unwrap_or(0);
            }
            GraphicsField::FrameLimit => {
                s.vsync = value == 0;
                s.fps = if value == 0 { 0 } else { LIMITS.get(value - 1).copied().unwrap_or(0) };
            }
            GraphicsField::ShowHud => s.show_hud = value != 0,
            GraphicsField::ShowFps => s.show_fps = value != 0,
            GraphicsField::MobileMode => {
                s.mobile = value != 0;
                // Mobile mode brings its lighter graphics; leaving it, the full look.
                let (textures, shadows, aa, scale) = if s.mobile { MOBILE_PRESET } else { (0, 2, 0, 100) };
                (s.texture_quality, s.shadows, s.anti_aliasing, s.scale) = (textures, shadows, aa, scale);
                TEXTURE_QUALITY.store(s.texture_quality, std::sync::atomic::Ordering::Relaxed);
                #[cfg(target_arch = "wasm32")]
                crate::web::set_mobile(s.mobile);
                self.status = "Mobile mode: textures change the next time a map loads.".into();
            }
            GraphicsField::PerformancePreset => {
                if value == 0 {
                    self.status = "Custom graphics settings kept.".into();
                } else {
                    let low_end = value == 2;
                    (s.texture_quality, s.shadows, s.anti_aliasing, s.scale) =
                        if low_end { (3, 0, 0, 50) } else { (0, 2, 0, 100) };
                    s.fps = if low_end { 60 } else { 0 };
                    s.vsync = false;
                    TEXTURE_QUALITY.store(s.texture_quality, std::sync::atomic::Ordering::Relaxed);
                    self.status = if low_end {
                        "Low-end PC preset applied. Render scale and frame limit apply now; reload the map for texture quality.".into()
                    } else {
                        "Balanced preset applied. Reload the map to restore full texture quality.".into()
                    };
                }
            }
        }
        let save = (|| -> Result<(), String> {
            skate_data::vfs::create_dir_all(self.path.parent().unwrap()).map_err(|e| e.to_string())?;
            skate_data::vfs::write(&self.path, serde_json::to_vec_pretty(&self.settings).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())
        })();
        if let Err(error) = save {
            self.status = format!("Could not save: {error}");
        }
    }
    pub(crate) fn diagnostic_settings(&self) -> String {
        format!("{:?}", self.settings)
    }
    /// The CrossBar's categories and rows for the current state, with what
    /// each row does. Rows mirror the panel's sections.
    pub(crate) fn crossbar(
        &self,
        net: &crate::multiplayer::Multiplayer,
        current_map: Option<&std::path::Path>,
        player: &crate::player_settings::PlayerSettings,
        pois: &[String],
        debug_cam: bool,
        modes: Option<&crate::game_modes::GameModes>,
    ) -> (crate::frontend_runtime::Model, Vec<CrossBarAction>) {
        use crate::frontend_runtime::{Category, Entry, Model};
        let mut actions = Vec::new();
        let mut row = |name: &str, icon: &str, help: &str, action: CrossBarAction, values: Vec<String>, value: usize| {
            actions.push(action);
            Entry {
                id: actions.len() as u32 - 1,
                name: format!("#{name}"),
                icon: icon.into(),
                help: format!("#{help}"),
                enabled: true,
                values: values.into_iter().map(|v| format!("#{v}")).collect(),
                value,
            }
        };
        let current = self.maps.iter().position(|m| m.path.as_deref() == current_map).unwrap_or(0);
        let web = cfg!(target_arch = "wasm32");

        // SKATE: the session, its map and how the board plays.
        let difficulty = Difficulty::ALL.iter().position(|d| *d == self.difficulty).unwrap_or(0);
        let worlds: Vec<String> = self.maps.iter().filter(|m| m.path.is_some()).map(|m| m.label.clone()).collect();
        if self.front_end {
            // The main menu: pick a world and drop in.
            let world = self.front_world.min(worlds.len().saturating_sub(1));
            let mut skate = vec![
                row("Freeskate", "start", &format!("Drop in at {}.", worlds.get(world).map_or("the world", |w| w.as_str())),
                    CrossBarAction::Skate, vec![], 0),
                row("World", "map", "Pick the part of Port Carverton to skate.", CrossBarAction::World, worlds.clone(), world),
                row("Difficulty", "settings", "How forgiving the board is. Custom opens the tuning sliders in game.",
                    CrossBarAction::Pick { page: Page::Section(1), row: 3, current: difficulty },
                    Difficulty::ALL.iter().map(|d| d.label().to_string()).collect(), difficulty),
            ];
            if !web {
                skate.push(row("Quit Game", "quit", "Exit to the desktop.", CrossBarAction::Row(Page::Section(2), 7), vec![], 0));
            }
            let skate = Category { name: "#FREESKATE".into(), icon: "singleplayer".into(), entries: skate };
            let online = Category {
                name: "#ONLINE".into(),
                icon: "multiplayer".into(),
                entries: vec![row("Join the Discord", "contacts",
                    "Find people to skate with, get help and hear about updates. Host or join a session from the pause menu once you are skating.",
                    CrossBarAction::Discord, vec![], 0),
                    row("Donate", "contacts", "Support the project.", CrossBarAction::Donate, vec![], 0),
                    row("Buy Me a Coffee", "contacts", "Support the project with a coffee.", CrossBarAction::Coffee, vec![], 0)],
            };
            let (player_category, options) = self.option_columns(player, &mut row);
            return (
                Model { title: "#MAIN MENU".into(), categories: vec![skate, online, player_category, options] },
                actions,
            );
        }
        let mut skate = vec![row("Resume", "start", "Back to your session.", CrossBarAction::Resume, vec![], 0)];
        skate.push(row("Map", "map", "Travel to another part of Port Carverton.", CrossBarAction::Travel,
            self.maps.iter().map(|m| m.label.clone()).collect(), current));
        if !pois.is_empty() {
            skate.push(row("Teleport", "map", "Drop in at a spot on this map.", CrossBarAction::Poi, pois.to_vec(), 0));
        }
        skate.push(row("Debug Camera", "view", "Fly the camera anywhere (F8). Press T, or B on a pad, to drop your skater at the camera.",
            CrossBarAction::DebugCam, vec![], 0));
        if debug_cam {
            skate.push(row("Teleport to Camera", "view", "Drop your skater where the debug camera is.", CrossBarAction::CameraTeleport, vec![], 0));
        }
        skate.push(row("Restart", "restart", "Reload this map from its spawn.", CrossBarAction::Row(Page::Map(current), 51), vec![], 0));
        skate.push(row("Difficulty", "settings", "How forgiving the board is. Custom opens the tuning sliders.",
            CrossBarAction::Pick { page: Page::Section(1), row: 3, current: difficulty },
            Difficulty::ALL.iter().map(|d| d.label().to_string()).collect(), difficulty));
        skate.push(row("Tuning Preset", "settings", "Load a ready-made Custom tuning: motorized speeds, big air, no bails and more.",
            CrossBarAction::Pick { page: Page::Section(1), row: 340, current: 0 },
            crate::custom_difficulty::PRESETS.iter().map(|p| p.name.to_string()).collect(), 0));
        if self.difficulty == Difficulty::Custom {
            skate.push(row("Custom Tuning", "settings", "Fine-tune every physics value.", CrossBarAction::Panel(Page::Section(1), 300), vec![], 0));
        }
        if !web {
            skate.push(row("Quit Game", "quit", "Exit to the desktop.", CrossBarAction::Row(Page::Section(2), 7), vec![], 0));
        }
        let skate = Category { name: "#SKATE".into(), icon: "singleplayer".into(), entries: skate };

        let mut multiplayer = Category {
            name: "#ONLINE".into(),
            icon: "multiplayer".into(),
            entries: if net.active() {
                vec![
                    row("Players", "players", &format!("Who is here and which map they're on. {}", net.status.lines().next().unwrap_or("")),
                        CrossBarAction::Players, net.player_list(), 0),
                    row("Player Name", "profile", "Change the name other skaters see.", CrossBarAction::Rename, vec![], 0),
                    row("Leave Session", "quitsession", "Leave multiplayer and keep skating alone.", CrossBarAction::Row(Page::Multiplayer(0), 5), vec![], 0),
                ]
                .into_iter()
                // Kicking, as in CS:GO: the host kicks, anyone else calls a vote.
                .chain(crate::multiplayer::kick::open_vote().map(|(_, name, ballot)| {
                    let current = match ballot { Some(false) => 1, _ => 0 };
                    row(&format!("Kick {name}?"), "players",
                        "Vote now: more than half of the other players must say yes. F1 yes, F2 no.",
                        CrossBarAction::KickBallot, vec!["Yes".into(), "No".into()], current)
                }))
                .chain({
                    let targets = crate::multiplayer::kick::menu_targets(net);
                    (!targets.is_empty()).then(|| {
                        let values: Vec<String> = std::iter::once("Choose a player".to_string())
                            .chain(targets.into_iter().map(|(_, name)| name))
                            .collect();
                        if net.is_host() {
                            row("Kick Player", "players", "Remove a player from the session. They can't rejoin it.",
                                CrossBarAction::Kick, values, 0)
                        } else {
                            row("Vote to Kick", "players",
                                "Call a vote to kick a player. More than half of the others must agree; a kicked player can't rejoin.",
                                CrossBarAction::Kick, values, 0)
                        }
                    })
                })
                .chain(net.session_clock().is_some_and(|c| c.1).then(|| {
                    let limits: Vec<String> = (0..=crate::object_dropper::MAX_LIMIT)
                        .map(|n| if n == 0 { "Off".to_string() } else { n.to_string() })
                        .collect();
                    row("Prop Limit", "parkeditor", "How many Object Dropper props each player can have out at once.",
                        CrossBarAction::PropLimit, limits, usize::from(crate::object_dropper::host_limit()))
                }))
                .chain(net.is_host().then(|| {
                    let (values, current) = max_players(net);
                    row("Max Players", "players", MAX_PLAYERS_HELP, CrossBarAction::MaxPlayers, values, current)
                }))
                .collect()
            } else {
                vec![
                    row("Host a Session", "hostoptions",
                        "Start a session your friends can join: free skate, or one of Skate 3's online game modes.",
                        CrossBarAction::HostMode,
                        std::iter::once("Freeskate".to_string())
                            .chain(host_modes(modes).into_iter().map(|k| k.label().to_string()))
                            .collect(),
                        0),
                    {
                        let (values, current) = max_players(net);
                        row("Max Players", "players", MAX_PLAYERS_HELP, CrossBarAction::MaxPlayers, values, current)
                    },
                    row("Find a Session", "skatewith", "Browse open sessions and join a crew.", CrossBarAction::Row(Page::Multiplayer(0), 6), vec![], 0),
                    row("Join with a Code", "contacts", "Type the connection code your friend sees.", CrossBarAction::Panel(Page::Multiplayer(1), 3), vec![], 0),
                    row("Player Name", "profile", "Change the name other skaters see.", CrossBarAction::Rename, vec![], 0),
                ]
            },
        };

        // Skate 3's online modes, at the game's own spots on this map: chosen
        // when hosting a session (or by its host), in two steps: the mode,
        // then the spot.
        if let Some((kind, _)) = self.mode_pick {
            let spots = modes.map_or_else(Vec::new, |m| m.spots(kind));
            multiplayer.entries = vec![
                row(&format!("{}: Choose a Spot", kind.label()), "players", mode_help(kind), CrossBarAction::Mode(kind),
                    std::iter::once("Random spot".to_string()).chain(spots).collect(), 0),
                row("Back", "quitsession", "Choose another mode.", CrossBarAction::ModeBack, vec![], 0),
            ];
        } else if let Some(kind) = modes.and_then(|m| m.active()) {
            if net.is_host() || !net.active() {
                multiplayer.entries.insert(0,
                    row(&format!("End {}", kind.label()), "quitsession", "Stop the round for everyone.", CrossBarAction::EndMode, vec![], 0));
            }
        } else if net.active() && net.is_host() {
            let kinds = host_modes(modes);
            if !kinds.is_empty() {
                multiplayer.entries.insert(0, row("Game Mode", "players",
                    "Start one of Skate 3's online game modes for everyone in the session.",
                    CrossBarAction::GameMode,
                    std::iter::once("Choose a mode".to_string()).chain(kinds.into_iter().map(|k| k.label().to_string())).collect(),
                    0));
            }
        }
        if self.mode_pick.is_none() {
            multiplayer.entries.push(row("Join the Discord", "contacts",
                "Find people to skate with, get help and hear about updates.", CrossBarAction::Discord, vec![], 0));
            multiplayer.entries.push(row("Donate", "contacts", "Support the project.", CrossBarAction::Donate, vec![], 0));
            multiplayer.entries.push(row("Buy Me a Coffee", "contacts", "Support the project with a coffee.", CrossBarAction::Coffee, vec![], 0));
        }

        let create = Category {
            name: "#CREATE".into(),
            icon: "create".into(),
            entries: vec![
                row("Object Dropper", "parkeditor", "Drop Skate 3's ramps, rails and props anywhere and skate them. In a session everyone sees them.",
                    CrossBarAction::ObjectDropper, vec![], 0),
                row("Create Skater", "profile", "Change your skater's look and gear.", CrossBarAction::Row(Page::Section(1), 8), vec![], 0),
                row("Custom Models", "parkeditor", "Bring your own models into the world.", CrossBarAction::Row(Page::Section(1), 10), vec![], 0),
                row("Mods", "extras", "Turn mods on or off and open their menus.", CrossBarAction::Row(Page::Section(4), 14), vec![], 0),
                row("Day & Night", "view", "Time of day and ambient light on custom maps.", CrossBarAction::Panel(Page::Daylight, 0), vec![], 0),
                row("Updates", "start", "Check for a newer build.", CrossBarAction::Row(Page::Section(4), 11), vec![], 0),
            ],
        };

        let (player_category, options) = self.option_columns(player, &mut row);

        let title = self
            .maps
            .iter()
            .find(|m| m.path.as_deref() == current_map)
            .map_or("FREESKATE".to_string(), |m| m.label.to_uppercase());
        (
            Model { title: format!("#{title}"), categories: vec![skate, multiplayer, create, player_category, options] },
            actions,
        )
    }
    /// The playable worlds (the test world left out), as the main menu lists them.
    pub(crate) fn worlds(&self) -> Vec<crate::map_library::Entry> {
        self.maps.iter().filter(|m| m.path.is_some()).cloned().collect()
    }
    /// Set the menu's difficulty by its index in `Difficulty::ALL`, as the
    /// Difficulty row would.
    pub(crate) fn choose_difficulty(&mut self, index: usize) {
        let current = Difficulty::ALL.iter().position(|d| *d == self.difficulty).unwrap_or(0);
        self.injected.push((Page::Section(1), 3, index as i32 - current as i32));
    }
    /// The PLAYER and GRAPHICS columns, shared by the pause and main menus.
    fn option_columns(
        &self,
        player: &crate::player_settings::PlayerSettings,
        row: &mut dyn FnMut(&str, &str, &str, CrossBarAction, Vec<String>, usize) -> crate::frontend_runtime::Entry,
    ) -> (crate::frontend_runtime::Category, crate::frontend_runtime::Category) {
        use crate::frontend_runtime::Category;
        let toggle = |on: bool| (vec!["Off".to_string(), "On".to_string()], usize::from(on));
        let index = |values: &[u32], v: u32| values.iter().position(|x| *x == v).unwrap_or(0);
        let web = cfg!(target_arch = "wasm32");
        let s = &self.settings;
        // PLAYER: camera and riding preferences, plus what is drawn over the game.
        let pick = CrossBarAction::Player;
        let fov = crate::player_settings::FOV_STEPS.iter().position(|f| *f == player.fov_percent).unwrap_or(4);
        let push = if player.auto_push { 1 + usize::from(player.push_right) } else { 0 };
        let (on_off, shake) = toggle(player.camera_shake);
        let graphics = CrossBarAction::Graphics;
        let player_category = Category {
            name: "#PLAYER".into(),
            icon: "learn".into(),
            entries: vec![
                row("Camera", "view", "The stock High camera, or the Low one closer to the board.", pick(PlayerField::Camera),
                    vec!["High".into(), "Low".into()], usize::from(player.low_camera)),
                row("Field of View", "view", "Wider or narrower than the original lens.", pick(PlayerField::FieldOfView),
                    crate::player_settings::FOV_STEPS.iter().map(|f| format!("{f}%")).collect(), fov),
                row("Camera Shake", "view", "The alternate shake when landing and at speed.", pick(PlayerField::Shake), on_off.clone(), shake),
                row("Invert Look", "settings", "Flip the right stick's look horizontally, vertically or both.", pick(PlayerField::InvertLook),
                    vec!["Off".into(), "Horizontal".into(), "Vertical".into(), "Both".into()],
                    usize::from(player.invert_x) + 2 * usize::from(player.invert_y)),
                row("Sound Volume", "start", "Game sound from your converted copy of Skate 3.", pick(PlayerField::Volume),
                    (0..=5).map(|v| if v == 0 { "Off".to_string() } else { format!("{}%", v * 20) }).collect(),
                    ((player.sound_volume + 10) / 20).min(5) as usize),
                row("Auto Push", "start", "Push automatically while riding, with the foot you choose.", pick(PlayerField::AutoPush),
                    vec!["Off".into(), "Left Foot".into(), "Right Foot".into()], push),
            ],
        };
        // The original column shows eight rows per tab. In the browser PLAYER
        // gains the controls card and Trackpad Mode (the page turns pointer
        // movement into the right stick), so the two display toggles move to
        // GRAPHICS; the desktop build reads controllers only and keeps them here.
        let mut player_category = player_category;
        let display = [
            row("Show HUD", "view", "The trick and score display.", graphics(GraphicsField::ShowHud), on_off.clone(), usize::from(s.show_hud)),
            row("FPS Counter", "settings", "The frame rate in the corner.", graphics(GraphicsField::ShowFps), on_off, usize::from(s.show_fps)),
        ];
        let mut display_rows = Vec::new();
        player_category.entries.insert(0, row("Trick Guide", "extras", "Every trick in the game and how to do it.",
            CrossBarAction::TrickGuide, vec![], 0));
        player_category.entries.insert(0, row("Controls", "settings", "Keyboard, trackpad and controller layout.",
            CrossBarAction::Controls, vec![], 0));
        if web {
            player_category.entries.push(row("Trackpad Mode", "settings",
                "Mouse or trackpad flicks tricks. Click the game to lock the pointer.",
                pick(PlayerField::Trackpad), vec!["Off".into(), "Low".into(), "Medium".into(), "High".into()], usize::from(player.trackpad)));
            display_rows.extend(display);
        } else {
            player_category.entries.extend(display);
        }

        // GRAPHICS: every option defaults to the original look.
        let size = (s.width, s.height);
        let mut options = Vec::new();
        options.push(row("Performance Preset", "settings",
            "Balanced restores the original graphics. Low-end PC lowers resolution, textures and shadows for smoother play; reload the map to apply textures.",
            graphics(GraphicsField::PerformancePreset),
            vec!["Custom".into(), "Balanced".into(), "Low-end PC".into()], s.performance_preset()));
        if !web {
            let resolution = RESOLUTIONS.iter().position(|r| *r == size).unwrap_or(0);
            options.push(row("Resolution", "settings", "Window size. Auto fits your screen.",
                CrossBarAction::Pick { page: Page::Section(2), row: 0, current: resolution },
                RESOLUTIONS.iter().map(|(w, h)| if *w == 0 { "Auto".to_string() } else { format!("{w} x {h}") }).collect(), resolution));
        }
        let (values, fullscreen) = toggle(s.fullscreen);
        options.push(row("Fullscreen", "view", "Borderless fullscreen at your display's resolution.",
            CrossBarAction::Pick { page: Page::Section(2), row: 4, current: fullscreen }, values, fullscreen));
        options.push(row("Render Scale", "settings", "Internal resolution. Lower is faster; 100% is the original sharpness.",
            CrossBarAction::Pick { page: Page::Section(2), row: 1, current: index(SCALES, s.scale) },
            SCALES.iter().map(|v| format!("{v}%")).collect(), index(SCALES, s.scale)));
        options.push(row("Texture Quality", "settings", "World texture resolution. High is the original; lower saves VRAM at the cost of detail. Applies when a map loads.",
            graphics(GraphicsField::TextureQuality),
            vec!["High".into(), "Medium".into(), "Low".into(), "Lowest".into()], s.texture_quality as usize));
        options.push(row("Shadows", "view", "Shadow detail from the sun and your skater. Medium is the original.",
            graphics(GraphicsField::Shadows),
            vec!["Off".into(), "Low".into(), "Medium".into(), "High".into()], s.shadows as usize));
        options.push(row("Anti-Aliasing", "settings", "Smooths jagged edges. Off is the original image.",
            graphics(GraphicsField::AntiAliasing),
            vec!["Off".into(), "FXAA".into(), "SMAA".into()], s.anti_aliasing as usize));
        options.push(row("Brightness", "view", "Overall image brightness. 100% is the original.",
            graphics(GraphicsField::Brightness),
            BRIGHTNESS.iter().map(|b| format!("{b}%")).collect(), index(BRIGHTNESS, s.brightness)));
        // Phone and tablet browsers close a page that uses too much memory.
        options.push(row("Mobile Mode", "settings", "For phones and tablets: lighter graphics, no background downloads and a warning before the big cities, so the browser keeps the game open.",
            graphics(GraphicsField::MobileMode),
            vec!["Off".into(), "On".into()], usize::from(s.mobile)));
        let label = |l: &u32| if *l == 0 { "Unlimited".to_string() } else { l.to_string() };
        if web {
            // A browser never draws faster than the display; a cap below it
            // saves battery and heat on laptops and Chromebooks.
            options.push(row("Frame Limit", "settings", "Cap the frame rate. Unlimited runs at your display's rate.",
                graphics(GraphicsField::FrameLimit),
                LIMITS.iter().map(label).collect(), index(LIMITS, s.fps)));
        } else {
            let limit = if s.vsync { 0 } else { 1 + index(LIMITS, s.fps) };
            options.push(row("Frame Limit", "settings", "VSync matches your display; otherwise cap or uncap the frame rate.",
                graphics(GraphicsField::FrameLimit),
                std::iter::once("VSync".to_string()).chain(LIMITS.iter().map(label)).collect(), limit));
        }
        options.extend(display_rows);
        let options = Category { name: "#GRAPHICS".into(), icon: "options".into(), entries: options };

        (player_category, options)
    }
    /// The panel is on one of its own pages (text entry, lobby browser, map
    /// spots, sliders), which the CrossBar hands back to it.
    pub(crate) fn panel_page(&self) -> bool {
        self.browser
            || self.network_page != 0
            || self.map_detail
            || self.daylight
            || (self.section == 1 && (300..337).contains(&self.selected))
    }
    /// Show the panel at a page the CrossBar cannot present.
    pub(crate) fn open_panel(&mut self, page: Page, selected: usize) {
        self.crossbar = false;
        match page {
            Page::Section(section) => {
                self.select_section(section);
                self.selected = selected;
            }
            Page::Multiplayer(network_page) => {
                self.select_section(3);
                self.network_page = network_page;
                self.selected = selected;
            }
            Page::Map(map) => {
                self.select_section(0);
                self.selected_map = map;
                self.map_detail = true;
                self.selected = selected;
            }
            Page::Daylight => {
                self.daylight = true;
                self.selected = selected;
            }
        }
    }
    pub(crate) fn transition_finished(&mut self, status: String, resume: bool) {
        self.status = status;
        self.open = !resume;
    }
}
/// The community Discord invite (pause menu > ONLINE > Join the Discord).
pub(crate) const DISCORD_INVITE: &str = "https://discord.gg/EKF8gZnZGe";
pub(crate) const DONATE_LINK: &str = "https://pump.fun/coin/BZDShqTtTL5EAk38RYSrFUfYM1YBZT4zqRajQz1Wpump";
pub(crate) const COFFEE_LINK: &str = "https://buymeacoffee.com/aaddpp";

/// Open a link in the player's browser: a new tab in the web build, the
/// default browser on desktop.
pub(crate) fn open_link(url: &str) {
    #[cfg(target_arch = "wasm32")]
    crate::web::open_url(url);
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "windows")))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(url).spawn();
    }
}

/// Ask for a name (Skate 3's popup, `name_prompt`); choosing it carries on
/// with `then`.
pub(crate) fn ask_name(menu: &mut Menu, then: Online) {
    menu.pending_online = Some(then);
    if then != Online::Rename {
        NAME_PROMPT.store(1, std::sync::atomic::Ordering::Relaxed);
    }
    menu.open = true;
    menu.status = "Choose a name other skaters will see, then press Enter.".into();
}

/// Without the original popup (no front-end export), the pause menu's own
/// name field asks instead; leaving it without a name drops what it held.
pub(crate) fn name_prompt_fallback(mut menu: ResMut<Menu>, popup: Option<Res<crate::name_prompt::Available>>) {
    if menu.pending_online.is_none() {
        return;
    }
    if !menu.open {
        menu.pending_online = None;
        menu.status.clear();
        cancel_name_prompt();
        return;
    }
    if popup.is_some() {
        return;
    }
    let showing = menu.multiplayer && menu.network_page == 2 && !menu.crossbar;
    if !showing {
        menu.browser = false;
        menu.open_panel(Page::Multiplayer(2), 7);
    }
}

/// The host's player limit, 2..=20 (every player's moves go through the
/// host, so more players need more of the host's upload).
fn max_players(net: &crate::multiplayer::Multiplayer) -> (Vec<String>, usize) {
    let values: Vec<String> = (2..=skate_net::lobby::MAX_PLAYERS).map(|n| n.to_string()).collect();
    (values, net.capacity().saturating_sub(2))
}
const MAX_PLAYERS_HELP: &str = "How many players your session holds. Big sessions need a fast upload: every player's moves go through the host.";

/// Joined a session on another map: travel there, then join it again (a map
/// change always leaves the session).
pub(crate) fn follow_host_map(
    menu: Res<Menu>,
    mut net: ResMut<crate::multiplayer::Multiplayer>,
    mut transition: ResMut<crate::map_transition::MapTransition>,
    mut asked: Local<Option<u64>>,
) {
    if transition.busy() {
        return;
    }
    if !net.active() {
        if let Some(lobby) = net.rejoin.take() {
            info!("MULTIPLAYER_REJOIN lobby={lobby}");
            net.join_lobby(lobby);
        }
        return;
    }
    if net.is_host() {
        return;
    }
    let host = net.host_actor();
    let map = net.player_map(host);
    if host == 0 || map.is_empty() || map == net.map_name() {
        return;
    }
    // Only sessions with a lobby to join again (Steam or the web relay).
    let Some(room) = net.room() else { return };
    if *asked == Some(room) {
        return;
    }
    *asked = Some(room);
    let label = |path: &std::path::Path| path.file_stem().map(|s| skate_net::directory::label(&s.to_string_lossy()));
    let Some(entry) = menu.maps.iter().find(|e| e.path.as_deref().and_then(label).is_some_and(|m| m == map)).cloned() else {
        net.status = format!("The host is skating {map}, which isn't on this install.");
        return;
    };
    info!("MULTIPLAYER_FOLLOW_MAP map={map} lobby={room}");
    net.status = format!("Travelling to {map} to join the session...");
    net.rejoin = Some(room);
    transition.request(entry);
}

/// The online game modes with spots on this map, in menu order.
pub(crate) fn host_modes(modes: Option<&crate::game_modes::GameModes>) -> Vec<crate::game_modes::Kind> {
    crate::game_modes::Kind::ALL
        .into_iter()
        .filter(|&kind| modes.is_some_and(|m| !m.spots(kind).is_empty()))
        .collect()
}

fn mode_help(kind: crate::game_modes::Kind) -> &'static str {
    use crate::game_modes::Kind;
    match kind {
        Kind::SpotBattle => "Take turns trying to land the highest scoring trick on the spot. Don't bail or your turn is over!",
        Kind::HallOfMeat => "Take turns trying to achieve the nastiest wipeout.",
        Kind::OneUp => "Set the score or beat it. Miss and you get a letter; spell 1-UP and you're out.",
        Kind::Skate => "Set a flip trick or grab on the spot, or copy the one set. Miss and you get a letter; spell S.K.A.T.E. and you're out.",
        Kind::Domination => "Own the most spots: land the best line on each one before time runs out.",
        Kind::Deathrace => "First to the finish wins. Follow the gates.",
    }
}

pub(crate) fn gameplay_active(menu: Option<Res<Menu>>) -> bool {
    menu.is_none_or(|m| !m.open) && !crate::object_dropper::is_open()
}

const SECTIONS: &[(&str, &str)] = &[
    ("MAPS", "Choose a map, then pick your drop-in spot."),
    ("SKATER", "Make it yours."),
    ("GRAPHICS", "Dial in your display and performance."),
    ("MULTIPLAYER", "A session is better with friends."),
    ("EXTRAS", "Mods, updates and more."),
];
#[derive(Component)] struct MenuTitle;
#[derive(Component)] struct MenuSubtitle;
#[derive(Component)] struct MenuScroll;
impl Menu {
    fn rows(&self) -> Vec<usize> {
        if self.daylight { return (0..4).collect(); }
        if self.multiplayer {
            return if self.browser { std::iter::once(0).chain(1..=self.browser_count.min(5)).chain([6,7,8,10]).collect() } else { match self.network_page {
                1 => vec![3,4,10],
                // Asked for a name on the way online: just the name.
                2 if self.pending_online.is_some() => vec![7,10],
                2 => vec![5,7,10],
                3 => vec![0,1,10],
                4 => (20..27).chain([10]).collect(),
                _ => vec![2,6,13,14,15],
            }};
        }
        match self.section {
            0 if self.map_detail => {
                let mut rows = vec![50, 51];
                if let Some(path) = self.maps.get(self.selected_map).and_then(|m| m.path.as_deref()) {
                    rows.extend(self.destinations.iter().enumerate().filter(|(_,d)| d.matrix.is_some() && crate::teleport_menu::same_map(path, &d.map)).map(|(i,_)| 1_000_000 + i));
                }
                rows
            }
            0 => (1000..1000 + self.maps.len()).collect(),
            1 if self.difficulty == Difficulty::Custom => std::iter::once(3).chain(300..337).chain([8,10]).collect(),
            1 => vec![3, 8, 10],
            2 => vec![0, 1, 2, 4, 13],
            4 => vec![7, 11, 14],
            i if i >= SECTIONS.len() => self.custom_sections.get(i-SECTIONS.len()).map_or(Vec::new(), |(_,entries)| (200..200+entries.len()).collect()),
            _ => Vec::new(),
        }
    }
    fn select_section(&mut self, section: usize) {
        self.section = section;
        self.map_detail = false;
        self.multiplayer = section == 3;
        self.browser = false;
        self.network_page = 0;
        self.daylight = false;
        self.selected = self.rows().first().copied().unwrap_or(100 + section);
        self.status.clear();
    }
}

#[derive(Resource)]
struct SceneTarget(Handle<Image>);
#[derive(Resource)]
struct FramePacer(Instant);
#[derive(Component)]
struct MenuRoot;
#[derive(Component)]
pub(crate) struct MenuLayoutRoot;
#[derive(Component)]
pub(crate) struct MenuRow(usize);
#[derive(Component)]
struct MenuLabel(usize);
#[derive(Component)]
struct StatusLabel;

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MenuInput;

/// The presentation camera must exist before overlays select their UI target.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PresentationSetup;

pub(crate) struct GraphicsMenuPlugin;
impl Plugin for GraphicsMenuPlugin {
    fn build(&self, app: &mut App) {
        // The world is built at Startup, before the menu's own setup; read the
        // saved texture quality now so the first map uses it.
        if let Some(config) = app.world().get_resource::<crate::config::Config>() {
            let path = config.asset_root.parent().unwrap_or(&config.asset_root).join("settings/graphics.json");
            let settings = GraphicsSettings::load(&path, config.browser);
            // A first start in mobile mode builds the first map with its textures.
            let quality = if config.mobile && !settings.mobile { MOBILE_PRESET.0 } else { settings.validated().texture_quality };
            TEXTURE_QUALITY.store(quality, std::sync::atomic::Ordering::Relaxed);
        }
        app.insert_resource(FramePacer(Instant::now()))
            .add_systems(PostStartup, setup.in_set(PresentationSetup))
            .add_systems(PreUpdate, (refresh_sections, interact, apply_custom_difficulty).chain().in_set(MenuInput).after(bevy::input::InputSystems))
            .add_systems(PreUpdate, finish_menu_travel.after(crate::map_transition::MapTransitionSet).before(crate::input::poll_controllers))
            .add_systems(Update, follow_host_map)
            .add_systems(Update, name_prompt_fallback)
            .add_systems(PreUpdate, toggle_fullscreen.after(bevy::input::InputSystems))
            .add_systems(Update, preview_menu.before(labels))
            .add_systems(Update, custom_sliders.before(labels))
            .add_systems(Update, (crate::map_render::advance_day, apply, labels, scroll_menu, resize_menu).chain())
            .add_systems(PostUpdate, crate::map_render::position_celestial_bodies.before(bevy::transform::TransformSystems::Propagate))
            .add_systems(Last, pace)
            .add_systems(Last, web_frame_limit.run_if(|| cfg!(target_arch = "wasm32")));
    }
}
fn setup(
    mut commands: Commands,
    config: Res<crate::config::Config>,
    mut images: ResMut<Assets<Image>>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    cameras: Query<Entity, With<Camera3d>>,
    mut time: ResMut<Time<Virtual>>,
) {
    let path = config
        .asset_root
        .parent()
        .unwrap_or(&config.asset_root)
        .join("settings/graphics.json");
    let settings = GraphicsSettings::load(&path, config.browser).validated();
    // In a browser the page decides (phones and tablets start in mobile mode;
    // the choice is kept there); natively, --mobile turns it on.
    let mut settings = settings;
    if cfg!(target_arch = "wasm32") || config.mobile {
        // First start in mobile mode: its lighter graphics come with it (the
        // player's own changes afterwards are kept, saved with `mobile` on).
        let first = config.mobile && !settings.mobile;
        if first {
            (settings.texture_quality, settings.shadows, settings.anti_aliasing, settings.scale) = MOBILE_PRESET;
        }
        settings.mobile = config.mobile;
        if first {
            // Saved, so the preset is applied once and the player's changes stay.
            let _ = skate_data::vfs::create_dir_all(path.parent().unwrap_or(&path));
            if let Ok(bytes) = serde_json::to_vec_pretty(&settings) {
                let _ = skate_data::vfs::write(&path, bytes);
            }
        }
    }
    if !settings.auto_size() {
        window
            .resolution
            .set_physical_resolution(settings.width, settings.height);
    }
    // A browser only enters fullscreen from a click or key press; the page asks
    // for it when Play is pressed instead.
    if settings.fullscreen && !cfg!(target_arch = "wasm32") {
        window.mode = WindowMode::BorderlessFullscreen(MonitorSelection::Primary);
    }
    window.present_mode = PresentMode::AutoNoVsync;
    let size = settings.internal_size(window.physical_size());
    let mut image = Image::new_target_texture(size.x, size.y, TextureFormat::Rgba8UnormSrgb, None);
    image.sampler = ImageSampler::linear();
    let target = images.add(image);
    for camera in &cameras {
        commands.entity(camera).insert((
            RenderTarget::Image(target.clone().into()),
            // Fixed policy, not a setting: see `render_capacity`.
            Msaa::Off,
        ));
        // Render the world before the presentation camera consumes its image.
        commands.entity(camera).insert(Camera {
            order: -1,
            ..default()
        });
    }
    let output = commands
        .spawn((Camera2d, Msaa::Off, IsDefaultUiCamera))
        .id();
    commands.spawn((
        Node {
            width: percent(100),
            height: percent(100),
            position_type: PositionType::Absolute,
            ..default()
        },
        ImageNode::new(target.clone()),
        UiTargetCamera(output),
    ));
    let data = skate_data::collections::Collections::load(&config.asset_root).expect("validated startup collections");
    let custom_defaults = Tuning::defaults(&data).expect("validated Easy difficulty");
    let custom = Tuning::load(&config.asset_root, &data).expect("validated Custom difficulty");
    let maps = crate::map_library::discover(&config.asset_root).unwrap_or_else(|error| {
        warn!("Map discovery failed: {error}");
        vec![crate::map_library::Entry {
            label: "Test world".into(),
            path: None,
        }]
    });
    let destinations = crate::teleport_menu::load(&config.asset_root).unwrap_or_else(|e| { warn!("Map destinations: {e}"); Vec::new() });
    commands.spawn((MenuRoot, MenuLayoutRoot, UiTargetCamera(output), GlobalZIndex(10), Node {
        display: Display::None, width:percent(100), height:percent(100), align_items:AlignItems::Center,
        justify_content:JustifyContent::Center, position_type:PositionType::Absolute, ..default()
    }, BackgroundColor(Color::srgba(0.015,0.02,0.025,0.90)))).with_children(|root| {
        root.spawn((Node { width:px(1180), height:px(700), flex_shrink:0., padding:UiRect::all(px(24)), column_gap:px(28), ..default() },
            BackgroundColor(Color::srgb(0.035,0.045,0.05)))).with_children(|panel| {
            panel.spawn(Node { width:px(210), flex_shrink:0., flex_direction:FlexDirection::Column, row_gap:px(8), overflow:Overflow::scroll_y(), ..default() }).with_children(|rail| {
                rail.spawn((Text::new("SKATE / 3"),TextFont {font_size:30.,..default()},TextColor(Color::srgb(0.78,0.96,0.3))));
                rail.spawn((Text::new("OFF THE BOARD"),TextFont {font_size:12.,..default()},TextColor(Color::srgb(0.55,0.62,0.62))));
                rail.spawn(Node {height:px(24),..default()});
                for i in 0..SECTIONS.len()+8 {
                    rail.spawn((Button,MenuRow(100+i),Node {width:percent(100),min_height:px(44),padding:UiRect::all(px(12)),align_items:AlignItems::Center,..default()},BackgroundColor(Color::NONE)))
                        .with_child((MenuLabel(100+i),Text::new(""),TextFont {font_size:16.,..default()},TextColor(Color::WHITE)));
                }
                rail.spawn((Text::new("ESC / START  /  RESUME"),TextFont {font_size:12.,..default()},TextColor(Color::srgb(0.55,0.62,0.62)),Node {margin:UiRect::top(px(20)),..default()}));
            });
            panel.spawn(Node {flex_grow:1.,min_width:px(0),flex_direction:FlexDirection::Column,row_gap:px(12),..default()}).with_children(|body| {
                body.spawn((Text::new("MAPS"),MenuTitle,TextFont {font_size:40.,..default()},TextColor(Color::WHITE)));
                body.spawn((Text::new(""),MenuSubtitle,TextFont {font_size:16.,..default()},TextColor(Color::srgb(0.65,0.72,0.72))));
                body.spawn((Node {height:px(3),width:px(64),margin:UiRect::bottom(px(10)),..default()},BackgroundColor(Color::srgb(0.78,0.96,0.3))));
                body.spawn((MenuScroll,ScrollPosition::default(),Node {flex_grow:1.,min_height:px(0),overflow:Overflow::scroll_y(),flex_direction:FlexDirection::Column,row_gap:px(8),..default()})).with_children(|list| {
                    for i in (0..4).chain(300..337).chain(4..10).chain(11..16).chain(20..27).chain([10]).chain(200..264).chain([50,51]).chain(1000..1000+maps.len()).chain(1_000_000..1_000_000+destinations.len()) {
                        list.spawn((Button,MenuRow(i),Node {flex_direction:if (300..335).contains(&i) {FlexDirection::Column} else {FlexDirection::Row},width:percent(100),min_height:px(56),flex_shrink:0.,padding:UiRect::axes(px(18),px(12)),align_items:AlignItems::Center,border_radius:BorderRadius::all(px(4)),..default()},BackgroundColor(Color::srgb(0.075,0.09,0.095))))
                            .with_children(|row| {
                                row.spawn((MenuLabel(i),Text::new(""),TextFont {font_size:18.,..default()},TextColor(Color::WHITE)));
                                if (300..335).contains(&i) && !OPTIONS[i-300].boolean {
                                    row.spawn((CustomTrack(i-300),RelativeCursorPosition::default(),Node {width:percent(100),height:px(14),margin:UiRect::top(px(8)),..default()},BackgroundColor(Color::srgb(0.15,0.19,0.20))))
                                        .with_child((CustomFill(i-300),Node {width:percent(0),height:percent(100),..default()},BackgroundColor(Color::srgb(0.65,0.85,0.25))));
                                }
                            });
                    }
                });
                body.spawn((StatusLabel,Text::new(""),TextFont {font_size:14.,..default()},TextColor(Color::srgb(0.78,0.96,0.3))));
                body.spawn((Text::new("Up/Down Navigate    Enter / A Select    Left/Right Adjust    Tab Sections\nCustom tuning: use Apply below. Other settings save automatically."),TextFont {font_size:13.,..default()},TextColor(Color::srgb(0.55,0.62,0.62))));
            });
        });
    });
    commands.insert_resource(SceneTarget(target));
    let selected_map = maps.iter().position(|m| m.path.as_ref() == config.map_path.as_ref()).unwrap_or(0);
    if config.start_paused { time.pause(); }
    commands.insert_resource(Menu {
        open: config.start_paused,
        selected: 1000,
        settings,
        path,
        difficulty: config.difficulty,
        custom, custom_defaults, custom_apply:false, custom_dirty:false,
        status: String::new(),
        maps,
        selected_map,
        multiplayer: false,
        browser: false, network_page: 0, browser_count: 0,
        daylight: false, section: 0, custom_sections: Vec::new(), map_detail: false, destinations, pending_travel: None,
        crossbar: false, controls_help: false, trick_guide: false, mode_pick: None, pending_online: None, injected: Vec::new(), front_end: false, front_world: 0, launch: false, hidden: false,
    });
}
fn cycle<T: PartialEq + Copy>(values: &[T], value: T, direction: i32) -> T {
    let index = values.iter().position(|x| *x == value).unwrap_or(0) as i32;
    values[(index + direction).rem_euclid(values.len() as i32) as usize]
}
fn refresh_sections(mods: Res<crate::modding::Mods>, mut menu: ResMut<Menu>) {
    let mut groups = std::collections::BTreeMap::<String,Vec<(String,String,String)>>::new();
    for ((owner,key),definition) in &mods.custom_menus {
        if let Some(section)=&definition.section {
            groups.entry(section.clone()).or_default().push((owner.clone(),key.clone(),definition.title.clone()));
        }
    }
    let selected=menu.custom_sections.get(menu.section.saturating_sub(SECTIONS.len())).filter(|_|menu.section>=SECTIONS.len()).map(|(name,_)|name.clone());
    menu.custom_sections=groups.into_iter().collect();
    if let Some(name)=selected {
        if let Some(i)=menu.custom_sections.iter().position(|(n,_)|n==&name) {menu.section=SECTIONS.len()+i;} else {menu.select_section(0);}
    }
}

pub(crate) fn interact(
    mut config: ResMut<crate::config::Config>,
    mut transition: ResMut<crate::map_transition::MapTransition>,
    mut customiser: ResMut<crate::customiser::Customiser>,
    mut custom_models: ResMut<crate::custom_models::CustomModels>,
    nav: Res<crate::customiser::Navigation>,
    mut physics: ResMut<crate::physics::GamePhysics>,
    keys: Res<ButtonInput<KeyCode>>,
    mut menu: ResMut<Menu>,
    mut time: ResMut<Time<Virtual>>,
    buttons: Query<(&Interaction, &MenuRow), Changed<Interaction>>,
    mut exit: MessageWriter<AppExit>,
    mut net: ResMut<crate::multiplayer::Multiplayer>,
    mut typing: MessageReader<bevy::input::keyboard::KeyboardInput>,
    mut updater: ResMut<crate::updater::Updater>,
    travel: Res<crate::teleport_menu::Travel>,
    (mut mods, chat): (ResMut<crate::modding::ModMenu>, Option<Res<crate::multiplayer::chat::Chat>>),
) {
    // The chat box has the keyboard (or just closed with Escape/Enter).
    if chat.as_ref().is_some_and(|c| c.has_keys()) {
        return;
    }
    if transition.busy() {
        menu.open = true;
        time.pause();
        return;
    }
    if travel.open || travel.closed_this_frame || customiser.open || custom_models.open || mods.open || menu.controls_help || menu.trick_guide
        || crate::object_dropper::has_keys() {
        return;
    }
    let crossbar = menu.open && menu.crossbar;
    if !crossbar && !menu.front_end && !menu.hidden && (keys.just_pressed(KeyCode::Escape) || nav.pressed & 0x10 != 0 || (menu.open && nav.pressed & 0x2000 != 0)) {
        if menu.open && menu.multiplayer && (menu.browser || menu.network_page != 0) {
            menu.browser = false; if menu.network_page==1 {menu.browser=true;menu.network_page=0;menu.selected=8;} else {let from_debug=menu.network_page==4;menu.network_page = 0; menu.selected = if from_debug {15} else {2};}
        } else if menu.open && menu.map_detail {
            menu.map_detail = false;
            menu.selected = 1000 + menu.selected_map;
        } else { menu.open = !menu.open; }
    }
    let mut action = None;
    for event in typing.read() {
        if !menu.open
            || crossbar
            || menu.hidden
            || !menu.multiplayer
            || menu.browser
            || !matches!(menu.selected, 3 | 7)
            || !event.state.is_pressed()
        {
            continue;
        }
        if menu.selected == 7 {
            let mut name = net.player_name.clone();
            if event.key_code == KeyCode::Backspace { name.pop(); }
            if let Some(text) = &event.text {
                for ch in text.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_')) {
                    if name.chars().count() < 16 { name.push(ch); }
                }
            }
            if name != net.player_name { net.set_player_name(name); }
            continue;
        }
        if event.key_code == KeyCode::Backspace {
            net.join_code.pop();
        }
        if let Some(text) = &event.text {
            for ch in text.chars().filter(|c| c.is_ascii_hexdigit() || *c == '-') {
                if net.join_code.len() < 40 {
                    net.join_code.push(ch);
                }
            }
        }
    }
    // While the front end shows its own screens the hidden panel takes no
    // keys; the CrossBar's choices below still run through it.
    if menu.open && !crossbar && !menu.hidden {
        menu.browser_count = net.browser_rows.len();
        if keys.just_pressed(KeyCode::Tab) {
            let direction = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) { SECTIONS.len()+menu.custom_sections.len()-1 } else { 1 };
            let section = (menu.section + direction) % (SECTIONS.len()+menu.custom_sections.len());
            menu.select_section(section);
        }
        let mut visible = menu.rows();
        visible.extend(100..100 + SECTIONS.len()+menu.custom_sections.len());
        let index = visible.iter().position(|r| *r == menu.selected).unwrap_or(0);
        let rows = visible.len();
        if keys.just_pressed(KeyCode::ArrowUp) || nav.pressed & 1 != 0 {
            menu.selected = visible[(index + rows - 1) % rows];
        }
        if keys.just_pressed(KeyCode::ArrowDown) || nav.pressed & 2 != 0 {
            menu.selected = visible[(index + 1) % rows];
        }
        let adjustable = (menu.section == 1 && menu.difficulty == Difficulty::Custom && (300..335).contains(&menu.selected)) || (menu.daylight && menu.selected < 3) || (!menu.multiplayer && !menu.daylight && menu.selected < 4);
        if adjustable && (keys.just_pressed(KeyCode::ArrowLeft) || nav.pressed & 4 != 0) {
            action = Some((menu.selected, -1));
        }
        if (adjustable && (keys.just_pressed(KeyCode::ArrowRight) || nav.pressed & 8 != 0))
            || (keys.just_pressed(KeyCode::Enter)
                && !keys.pressed(KeyCode::AltLeft)
                && !keys.pressed(KeyCode::AltRight))
            || nav.pressed & 0x1000 != 0
        {
            action = Some((menu.selected, 1));
        }
        for (interaction, row) in &buttons {
            if *interaction == Interaction::Pressed && visible.contains(&row.0) {
                menu.selected = row.0;
                action = Some((row.0, 1));
            }
        }
    }
    // CrossBar choices run as the panel row they stand for.
    if action.is_none() && !menu.injected.is_empty() {
        let (page, row, direction) = menu.injected.remove(0);
        match page {
            Page::Section(section) => menu.select_section(section),
            Page::Multiplayer(network_page) => {
                menu.select_section(3);
                menu.network_page = network_page;
            }
            Page::Map(map) => {
                menu.select_section(0);
                menu.selected_map = map;
            }
            Page::Daylight => menu.daylight = true,
        }
        menu.selected = row;
        action = Some((row, direction));
    }
    if let Some((row, _)) = action {
        if (100..100 + SECTIONS.len()+menu.custom_sections.len()).contains(&row) {
            menu.select_section(row - 100);
            action = None;
        } else if (200..264).contains(&row) {
            if let Some((_,entries))=menu.custom_sections.get(menu.section.saturating_sub(SECTIONS.len())) {
                if let Some((owner,key,_))=entries.get(row-200) {mods.open_registered(owner.clone(),key.clone());}
            }
            action=None;
        } else if row == 50 {
            menu.map_detail = false;
            menu.selected = 1000 + menu.selected_map;
            menu.status.clear();
            action = None;
        } else if row == 51 || row >= 1_000_000 {
            if let Some(entry) = menu.maps.get(menu.selected_map).cloned() {
                let matrix = row.checked_sub(1_000_000).and_then(|i| menu.destinations.get(i)).and_then(|d| d.matrix);
                if net.active() && entry.path != config.map_path {
                    menu.status = "Leave multiplayer before switching maps".into();
                } else if let Some(matrix) = matrix {
                    menu.pending_travel = Some((entry.path.clone(), matrix));
                    if entry.path != config.map_path { transition.request(entry); }
                    menu.status = "Travelling to your spot...".into();
                } else if row == 51 {
                    if net.active() { menu.status = "Leave multiplayer before reloading a map".into(); }
                    else { transition.request(entry); menu.status = "Loading map...".into(); }
                }
            }
            action = None;
        } else if row >= 1000 {
            menu.selected_map = row - 1000;
            menu.map_detail = true;
            menu.selected = 51;
            menu.status.clear();
            action = None;
        }
    }

    if let Some((row, direction)) = action {
        let day_action = menu.daylight;
        if menu.daylight {
            match row {
                0 => menu.settings.hour = ((menu.settings.hour * 4.).round() + direction as f32).rem_euclid(96.) / 4.,
                1 => menu.settings.day_speed = cycle(DAY_SPEEDS, menu.settings.day_speed, direction),
                2 => {
                    // Auto, 0%, 5%, ... 100%, then Auto again.
                    let index = menu.settings.ambient_level.map_or(0, |level| level as i32 / 5 + 1);
                    let next = (index + direction).rem_euclid(22);
                    menu.settings.ambient_level = if next == 0 { None } else { Some((next as u32 - 1) * 5) };
                }
                _ => { menu.daylight = false; menu.selected = 13; }
            }
        } else if menu.browser {
            match row {
                0 => net.browse(0),
                1..=5 if net.needs_name() => ask_name(&mut menu, Online::Browser(row - 1)),
                1..=5 => net.join_row(row - 1),
                6 => {
                    let page = net.browser_page.saturating_sub(1);
                    net.browse(page);
                }
                7 => {
                    let page = net.browser_page + 1;
                    if page * 5 < net.browser_total {
                        net.browse(page);
                    }
                }
                8 => { menu.browser=false;menu.network_page=1;menu.selected=3; },
                9 => {
                    exit.write(AppExit::Success);
                }
                10 => {
                    menu.browser = false;
                    menu.selected = 6;
                }
                _ => {}
            }
        } else if menu.multiplayer {
            match row {
                // Going online: a name first, if it is still the default.
                0 | 1 | 2 | 4 if net.needs_name() => ask_name(&mut menu, Online::Row(row)),
                // Enter on the name: carry on where the player was going.
                7 if menu.pending_online.is_some() => {
                    if net.needs_name() {
                        menu.status = "Type a name other skaters will see (not \"Player\"), then press Enter.".into();
                    } else if let Some(online) = menu.pending_online.take() {
                        clear_name_prompt();
                        menu.status.clear();
                        match online {
                            Online::Row(row) => menu.injected.push((Page::Multiplayer(0), row, 1)),
                            Online::Browser(index) => net.join_row(index),
                            Online::Rename => {}
                        }
                        menu.open = false;
                    }
                }
                0 => net.local(true),
                1 => net.local(false),
                2 => net.steam(true),
                3 => {}
                4 => net.steam(false),
                5 => net.leave(),
                6 => {
                    net.browse(0);
                    menu.browser = true;
                    menu.selected = 0;
                }
                12 => { menu.browser=false;menu.network_page=1;menu.selected=3; },
                8 => menu.open = false,
                9 => {
                    exit.write(AppExit::Success);
                }
                10 => {
                    if menu.network_page==1 {menu.browser=true;menu.network_page=0;menu.selected=8;} else {let from_debug=menu.network_page==4;menu.network_page = 0; menu.selected = if from_debug {15} else {2};}
                }
                13 => { menu.network_page = 2; menu.selected = 7; }
                14 => { menu.network_page = 3; menu.selected = 0; }
                15 => { menu.network_page = 4; menu.selected = 20; }
                _ => {}
            }
        } else {
            match row {
                300..=334 => { menu.custom.adjust(row-300,direction); menu.custom_dirty=true; },
                335 => menu.custom_apply=true,
                336 => { menu.custom=menu.custom_defaults.clone(); menu.custom_dirty=true; menu.status="Easy values restored. Select Apply custom tuning to use them.".into(); },
                0 => {
                    let size = cycle(
                        RESOLUTIONS,
                        (menu.settings.width, menu.settings.height),
                        direction,
                    );
                    (menu.settings.width, menu.settings.height) = size;
                }
                1 => menu.settings.scale = cycle(SCALES, menu.settings.scale, direction),
                2 => menu.settings.fps = cycle(LIMITS, menu.settings.fps, direction),
                4 => menu.settings.fullscreen = !menu.settings.fullscreen,
                // CrossBar tuning presets: the step carries the preset index.
                340 => {
                    if let Some(preset) = crate::custom_difficulty::PRESETS.get(direction.max(0) as usize) {
                        menu.custom = preset.tuning(&menu.custom_defaults);
                        menu.difficulty = Difficulty::Custom;
                        physics.set_difficulty(menu.difficulty);
                        config.difficulty = menu.difficulty;
                        let _ = menu.difficulty.save(&config.asset_root);
                        menu.custom_dirty = true;
                        menu.custom_apply = true;
                        menu.status = format!("{} preset loaded", preset.name);
                    }
                }
                3 => {
                    menu.difficulty = cycle(&Difficulty::ALL, menu.difficulty, direction);
                    physics.set_difficulty(menu.difficulty);
                    config.difficulty = menu.difficulty;
                    menu.status = match menu.difficulty.save(&config.asset_root) {
                        Ok(()) => "Difficulty saved".into(),
                        Err(e) => format!("Applied, but could not save: {e}"),
                    };
                }
                6 => menu.open = false,
                7 => {
                    exit.write(AppExit::Success);
                }
                8 => { custom_models.request_stock(); customiser.begin(); },
                9 => {
                    menu.multiplayer = true;
                    menu.selected = 0;
                }
                10 => custom_models.begin(),
                11 => menu.status = updater.open(false),
                12 => {
                    menu.select_section(0);
                    menu.selected_map = menu.maps.iter().position(|m| m.path == config.map_path).unwrap_or(0);
                    menu.map_detail = true;
                    menu.selected = 51;
                },
                13 => { menu.daylight = true; menu.selected = 0; menu.status = "Custom maps: change time, cycle speed and ambient light. Retail lighting stays authored.".into(); },
                14 => mods.begin(),
                _ => {}
            }
        }
        if ((row < 3 || row == 4) && !menu.multiplayer && !menu.daylight && !day_action) || (day_action && row < 3) {
            let save = (|| -> Result<(), String> {
                skate_data::vfs::create_dir_all(menu.path.parent().unwrap()).map_err(|e| e.to_string())?;
                skate_data::vfs::write(
                    &menu.path,
                    serde_json::to_vec_pretty(&menu.settings).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())
            })();
            menu.status = match save {
                Ok(()) => "Saved".into(),
                Err(e) => format!("Could not save: {e}"),
            };
        }
    }
    if menu.open && !net.active() {
        time.pause();
    } else {
        time.unpause();
    }
}
/// Alt+Enter flips the fullscreen setting; `apply` moves the window.
fn toggle_fullscreen(keys: Res<ButtonInput<KeyCode>>, mut menu: ResMut<Menu>) {
    let alt = keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight);
    if alt && keys.just_pressed(KeyCode::Enter) {
        menu.settings.fullscreen = !menu.settings.fullscreen;
    }
}
fn apply(
    mut commands: Commands,
    menu: Res<Menu>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    target: Res<SceneTarget>,
    mut images: ResMut<Assets<Image>>,
    mut previous: Local<Option<GraphicsSettings>>,
    cameras: Query<(Entity, Has<bevy::anti_alias::fxaa::Fxaa>, Has<bevy::anti_alias::smaa::Smaa>), With<Camera3d>>,
) {
    let s = &menu.settings;
    // Anti-aliasing runs on the world camera after the retail tone pass.
    for (camera, fxaa, smaa) in &cameras {
        let want = (s.anti_aliasing == 1, s.anti_aliasing == 2);
        if (fxaa, smaa) != want {
            let mut camera = commands.entity(camera);
            camera.remove::<(bevy::anti_alias::fxaa::Fxaa, bevy::anti_alias::smaa::Smaa)>();
            match s.anti_aliasing {
                1 => { camera.insert(bevy::anti_alias::fxaa::Fxaa::default()); }
                2 => { camera.insert(bevy::anti_alias::smaa::Smaa::default()); }
                _ => {}
            }
        }
    }
    if previous.as_ref().is_none_or(|p| p.shadows != s.shadows) {
        commands.insert_resource(bevy::light::DirectionalLightShadowMap {
            size: SHADOW_SIZES[s.shadows as usize],
        });
    }
    if !cfg!(target_arch = "wasm32") && previous.as_ref().is_none_or(|p| p.vsync != s.vsync) {
        window.present_mode = if s.vsync { PresentMode::AutoVsync } else { PresentMode::AutoNoVsync };
    }
    let first = previous.is_none();
    let fullscreen_changed = previous.as_ref().is_some_and(|p| p.fullscreen != s.fullscreen);
    // Startup already chose the mode; afterwards only a change moves the window
    // (a browser enters fullscreen from the key press that chose it).
    if fullscreen_changed {
        window.mode = if s.fullscreen {
            WindowMode::BorderlessFullscreen(MonitorSelection::Primary)
        } else {
            WindowMode::Windowed
        };
    }
    let size_changed = previous.as_ref().is_none_or(|p| (p.width, p.height) != (s.width, s.height));
    if window.mode == WindowMode::Windowed && !s.auto_size() && (size_changed || fullscreen_changed) {
        window.resolution.set_physical_resolution(s.width, s.height);
    }
    // Auto in a desktop window: use the screen the window is on.
    if !cfg!(target_arch = "wasm32") && window.mode == WindowMode::Windowed && s.auto_size()
        && (first || fullscreen_changed || size_changed)
    {
        window.set_maximized(true);
    }
    let size = s.internal_size(window.physical_size());
    if let Some(image) = images.get(&target.0) {
        if image.size() != size {
            images.get_mut(&target.0).unwrap().resize(Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: 1,
            });
        }
    }
    *previous = Some(menu.settings.clone());
}
fn labels(
    menu: Res<Menu>,
    transition: Res<crate::map_transition::MapTransition>,
    time: Res<Time<Real>>,
    customiser: Res<crate::customiser::Customiser>,
    custom_models: Res<crate::custom_models::CustomModels>,
    travel: Res<crate::teleport_menu::Travel>,
    mods: Res<crate::modding::ModMenu>,
    net: Res<crate::multiplayer::Multiplayer>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut root: Single<&mut Node, With<MenuRoot>>,
    mut labels: Query<(&MenuLabel, &mut Text), (Without<StatusLabel>, Without<MenuTitle>, Without<MenuSubtitle>)>,
    mut headings: Query<(&mut Text, Has<MenuTitle>), (Or<(With<MenuTitle>, With<MenuSubtitle>)>, Without<StatusLabel>)>,
    mut status: Single<&mut Text, With<StatusLabel>>,
    debug: (Res<crate::modding::Mods>, Res<crate::physics::GamePhysics>, Res<crate::multiplayer::appearance::Appearances>),
    mut buttons: Query<(&MenuRow, &Interaction, &mut BackgroundColor, &mut Node), Without<MenuRoot>>,
) {
    // Only on change: this runs every frame, and any Node write re-runs UI
    // layout for that frame.
    let display = if menu.open && !menu.hidden && !menu.crossbar && !menu.controls_help && !menu.trick_guide && !travel.open && !customiser.open && !custom_models.open && !mods.open {
        Display::Flex
    } else {
        Display::None
    };
    if root.display != display {
        root.display = display;
    }
    if !menu.open {
        return;
    }
    for (mut text, title) in &mut headings {
        **text = if menu.map_detail {
            if title { menu.maps.get(menu.selected_map).map_or("MAP", |m| m.label.as_str()) }
            else { "Choose a teleport spot, or skate from the default spawn." }
        } else if menu.daylight {
            if title { "DAY & NIGHT" } else { "Custom maps: time and ambient light. Retail lighting stays authored." }
        } else if menu.browser {
            if title { "FIND A SESSION" } else { "Browse public lobbies and join a crew." }
        } else if menu.multiplayer && menu.network_page != 0 {
            match (menu.network_page, title) {
                (1,true) => "JOIN A FRIEND", (1,false) => "Select the code field, type your friend's code, then choose Join session.",
                (2,true) if menu.pending_online.is_some() => "CHOOSE YOUR NAME",
                (2,false) if menu.pending_online.is_some() => "Type the name other skaters will see, then press Enter to go online.",
                (2,true) => "PLAYER & SESSION", (2,false) => "Select your name to edit it. Leave your current session here.",
                (4,true) => "MULTIPLAYER DEBUG", (4,false) => "Live network and mod diagnostics. Scroll or use Up/Down to inspect.",
                (3,true) => "LOCAL TESTING", (_,false) => "Advanced: host or join a local test session without Steam.",
                _ => "MULTIPLAYER",
            }
        } else if title { SECTIONS.get(menu.section).map_or_else(||menu.custom_sections.get(menu.section-SECTIONS.len()).map_or("",|(n,_)|n.as_str()),|s|s.0) } else { SECTIONS.get(menu.section).map_or("Choose an activity.",|s|s.1) }.into();
    }
    let mut debug_rows = Vec::new();
    if menu.multiplayer && menu.network_page == 4 {
        debug_rows.extend(net.debug_sections());
        debug_rows.push(format!("CHARACTERS & COLLISIONS\n{} {}\nPlayer contacts: {} | Network active: {}",
            debug.2.progress, debug.2.status, debug.1.network_contacts, debug.1.network_active));
        debug_rows.extend(debug.0.multiplayer_debug_sections());
    }
    let s = &menu.settings;
    let size = s.internal_size(window.physical_size());
    for (label, mut text) in &mut labels {
        **text = if (100..113).contains(&label.0) {
            let i=label.0-100;
            let name=SECTIONS.get(i).map(|s|s.0).or_else(||menu.custom_sections.get(i.saturating_sub(SECTIONS.len())).map(|(n,_)|n.as_str())).unwrap_or("");
            format!("{:02}   {}",i+1,name)
        } else if (200..264).contains(&label.0) {
            menu.custom_sections.get(menu.section.saturating_sub(SECTIONS.len())).and_then(|(_,e)|e.get(label.0-200)).map(|(_,_,t)|t.clone()).unwrap_or_default()
        } else if label.0 >= 1_000_000 {
            menu.destinations.get(label.0 - 1_000_000).map(|d| d.name.clone()).unwrap_or_default()
        } else if label.0 == 50 { "<  All maps".into()
        } else if label.0 == 51 { "Skate from default spawn".into()
        } else if label.0 >= 1000 {
            menu.maps.get(label.0 - 1000).map(|entry| format!("{}    /    VIEW SPOTS", entry.label)).unwrap_or_default()
        } else if menu.multiplayer && menu.network_page == 4 && (20..27).contains(&label.0) {
            debug_rows.get(label.0 - 20).cloned().unwrap_or_default()
        } else if menu.daylight {
            match label.0 {
                0 => { let minutes = (s.hour * 60.).floor() as u32 % 1440; format!("Time of day          {:02}:{:02}", minutes / 60, minutes % 60) },
                1 => if s.day_speed == 0 { "Cycle speed          Frozen".into() } else { format!("Cycle speed          {}x ({} min/day)", s.day_speed, 1440 / s.day_speed) },
                2 => match s.ambient_level {
                    Some(level) => format!("Ambient light        {level}%"),
                    None => "Ambient light        Auto (day/night)".into(),
                },
                3 => "Back".into(),
                _ => String::new(),
            }
        } else if menu.browser {
            match label.0 {
                0 => "Refresh sessions".into(),
                1..=5 => net
                    .browser_rows
                    .get(label.0 - 1)
                    .map(|r| {
                        format!(
                            "{} | {}/{} | #{}{}",
                            r.map,
                            r.players,
                            r.capacity,
                            r.id % 100000,
                            if r.compatible {
                                ""
                            } else if r.map.split(" | ").next() != Some(net.map_name()) {
                                // Another map: joining travels there.
                                " | Other map: you'll travel there"
                            } else {
                                " | Update required"
                            }
                        )
                    })
                    .unwrap_or_else(|| "--".into()),
                6 => "Previous page".into(),
                7 => "Next page".into(),
                8 => "Join with a code".into(),
                9 => "Quit game".into(),
                _ => "Back to multiplayer".into(),
            }
        } else if menu.multiplayer {
            match label.0 {
                0 => "Host local session".into(),
                1 => "Join local session".into(),
                2 => "Host a session".into(),
                3 => format!(
                    "Join code: {}{}",
                    net.join_code,
                    if menu.selected == 3 { "_" } else { "" }
                ),
                4 => "Join session".into(),
                5 => "Leave multiplayer".into(),
                6 => "Find a session".into(),
                7 => format!("Your name: {}{}", net.player_name, if menu.selected == 7 { "_" } else { "" }),
                12 => "Join with a code".into(),
                8 => "Resume".into(),
                9 => "Quit game".into(),
                13 => "Player & session".into(),
                14 => "Advanced / local testing".into(),
                15 => "Debug".into(),
                _ => "<  Multiplayer".into(),
            }
        } else {
            match label.0 {
                0 if s.auto_size() => "Resolution          Auto".into(),
                0 => format!("Resolution          {} x {}", s.width, s.height),
                4 => format!("Fullscreen          {}", if s.fullscreen { "On" } else { "Off" }),
                1 => format!(
                    "Internal resolution   {}%  ({} x {})",
                    s.scale, size.x, size.y
                ),
                2 => format!(
                    "FPS limit             {}",
                    if s.fps == 0 {
                        "Unlimited".into()
                    } else {
                        s.fps.to_string()
                    }
                ),
                3 => format!("Difficulty            {}", menu.difficulty.label()),
                300..=334 => menu.custom.label(label.0-300),
                335 => if menu.custom_dirty {"Apply custom tuning *".into()} else {"Apply custom tuning".into()},
                336 => "Reset custom tuning to Easy".into(),
                6 => "Resume".into(),
                7 => "Quit game".into(),
                8 => "Character customiser".into(),
                10 => "Custom models".into(),
                11 => "Updates".into(),
                12 => "Teleport".into(),
                13 => "Day & night".into(),
                14 => "Mods".into(),
                _ => "Multiplayer".into(),
            }
        };
    }
    ***status = if transition.busy() {
        format!("{} {}\nGameplay is paused. Please wait.", ["|", "/", "-", "\\"][(time.elapsed_secs() * 4.) as usize % 4], transition.label())
    } else if menu.multiplayer && menu.network_page == 4 {
        "Diagnostics stay in this menu; gameplay shows names and ping only.".into()
    } else if menu.pending_online.is_some() && !menu.status.is_empty() {
        menu.status.clone()
    } else if menu.browser {
        net.browser_status.clone()
    } else if menu.multiplayer {
        format!(
            "{}{}",
            net.status,
            if net.host_code.is_empty() {
                String::new()
            } else {
                format!("\nYour connection code: {}", net.host_code)
            }
        )
    } else if menu.section == 1 && menu.difficulty == Difficulty::Custom && (300..335).contains(&menu.selected) {
        format!("{}\n{}", OPTIONS[menu.selected-300].description,
            if menu.custom_dirty {"Changes pending - select Apply custom tuning below. Left/Right adjusts; drag bars with the mouse."} else {"Left/Right adjusts; drag bars with the mouse. Custom starts from Easy."})
    } else {
        menu.status.clone()
    };
    let visible = menu.rows();
    for (row, interaction, mut color, mut node) in &mut buttons {
        node.display = if visible.contains(&row.0) || (100..100+SECTIONS.len()+menu.custom_sections.len()).contains(&row.0) { Display::Flex } else { Display::None };
        color.0 = if row.0 == menu.selected || row.0 == 100 + menu.section || *interaction == Interaction::Hovered {
            Color::srgb(0.24, 0.33, 0.12)
        } else {
            Color::srgb(0.075, 0.09, 0.095)
        };
    }
}
// Opt-in screenshot coverage for overlays; never changes an ordinary session.
fn preview_menu(config: Res<crate::config::Config>, mut menu: ResMut<Menu>, mut mods: ResMut<crate::modding::ModMenu>, mut done: Local<bool>) {
    if *done || config.verification_capture.is_none() { return; }
    *done = true;
    match std::env::var("SKATE_VERIFY_MENU").as_deref() {
        Ok("custom") => { menu.open = true; menu.select_section(1); menu.selected=300; },
        Ok("mods") => { menu.open = true; mods.begin(); }
        Ok("multiplayer") => { menu.open = true; menu.select_section(3); }
        _ => {}
    }
}
fn finish_menu_travel(
    mut menu: ResMut<Menu>, transition: Res<crate::map_transition::MapTransition>,
    current: Res<crate::map_transition::CurrentMap>, mut skater: ResMut<crate::physics::SkaterRuntime>,
    mut time: ResMut<Time<Virtual>>,
) {
    if transition.busy() { return; }
    let Some((path, matrix)) = menu.pending_travel.take() else { return; };
    // A failed map transaction retains the previous world: never apply another map's coordinates there.
    if current.path != path { return; }
    match skater.travel_to(matrix) {
        Ok(()) => { menu.open = false; time.unpause(); }
        Err(e) => { menu.open = true; menu.status = format!("Could not travel: {e}"); }
    }
}
fn resize_menu(
    window: Single<&Window, With<PrimaryWindow>>, roots: Query<Entity, With<MenuLayoutRoot>>,
    children: Query<&Children>, mut nodes: Query<(&mut Node, Option<&mut TextFont>)>,
    mut previous: Local<Option<f32>>,
) {
    let scale = (window.width() / 1280.).min(window.height() / 800.).max(0.25);
    let factor = scale / previous.unwrap_or(1.);
    if (factor - 1.).abs() < 0.0001 { return; }
    fn resize(value: &mut Val, factor: f32) { if let Val::Px(px) = value { *px *= factor; } }
    for root in &roots {
        for entity in children.iter_descendants(root) {
            if let Ok((mut node, font)) = nodes.get_mut(entity) {
                let node = &mut *node;
                for value in [&mut node.width, &mut node.height, &mut node.min_width, &mut node.min_height,
                    &mut node.max_width, &mut node.max_height, &mut node.row_gap, &mut node.column_gap,
                    &mut node.padding.left, &mut node.padding.right, &mut node.padding.top, &mut node.padding.bottom,
                    &mut node.margin.left, &mut node.margin.right, &mut node.margin.top, &mut node.margin.bottom] {
                    resize(value, factor);
                }
                if let Some(mut font) = font { font.font_size *= factor; }
            }
        }
    }
    *previous = Some(scale);
}
fn scroll_menu(
    menu: Res<Menu>,
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    mut scroll: Query<(&mut ScrollPosition, &ComputedNode, &Node), With<MenuScroll>>,
    rows: Query<(&MenuRow, &ComputedNode)>,
    mut previous: Local<Option<(usize, bool, bool, usize, bool, u8)>>,
) {
    let delta: f32 = wheel.read().map(|e| e.y * if e.unit == bevy::input::mouse::MouseScrollUnit::Line { 40. } else { 1. }).sum();
    if !menu.open { return; }
    let state = (menu.section, menu.daylight, menu.browser, menu.selected, menu.map_detail, menu.network_page);
    let visible = menu.rows();
    for (mut pos, node, layout) in &mut scroll {
        let scale = node.inverse_scale_factor();
        let height = node.size().y * scale;
        let max = (node.content_size().y * scale - height).max(0.);
        if previous.as_ref().is_none_or(|p| (p.0,p.1,p.2,p.4,p.5) != (state.0,state.1,state.2,state.4,state.5)) { pos.y = 0.; }
        else if previous.as_ref() != Some(&state) {
            let mut top = 0.;
            for id in &visible {
                let row_height = rows.iter().find(|(r,_)| r.0 == *id).map_or(56., |(_,n)| n.size().y * scale);
                if *id == menu.selected {
                    if top < pos.y { pos.y = top; }
                    else if top + row_height > pos.y + height { pos.y = top + row_height - height; }
                    break;
                }
                top += row_height + if let Val::Px(gap) = layout.row_gap { gap } else { 0. };
            }
        }
        pos.y = (pos.y - delta).clamp(0., max);
    }
    *previous = Some(state);
}
/// Browser: a wasm thread cannot sleep, so a cap is the app waking once per
/// frame period (input waits for the next frame rather than waking it early).
fn web_frame_limit(menu: Option<Res<Menu>>, mut winit: ResMut<bevy::winit::WinitSettings>, mut applied: Local<Option<(u32, u32)>>) {
    let fps = menu.map_or(0, |m| m.settings.fps);
    #[cfg(target_arch = "wasm32")]
    let hz = crate::web::display_hz().round() as u32;
    #[cfg(not(target_arch = "wasm32"))]
    let hz = 0;
    if *applied == Some((fps, hz)) {
        return;
    }
    *applied = Some((fps, hz));
    // At or above the display's rate the browser's own pacing is the cap; a
    // timer racing it only drops frames (75 on a 60 Hz screen gave 52).
    let mode = if fps == 0 || (hz > 0 && fps + 2 >= hz) {
        bevy::winit::UpdateMode::Continuous
    } else {
        bevy::winit::UpdateMode::Reactive {
            wait: Duration::from_secs_f64(1. / f64::from(fps)),
            react_to_device_events: false,
            react_to_user_events: false,
            react_to_window_events: false,
        }
    };
    winit.focused_mode = mode;
    winit.unfocused_mode = mode;
}

fn pace(menu: Option<Res<Menu>>, mut pacer: ResMut<FramePacer>) {
    let Some(menu) = menu else {
        return;
    };
    // The browser paces frames to the display itself, and a wasm32 thread
    // cannot sleep: the limiter would abort the game there.
    if menu.settings.fps > 0 && !cfg!(target_arch = "wasm32") {
        let period = Duration::from_secs_f64(1. / f64::from(menu.settings.fps));
        if let Some(wait) = period.checked_sub(pacer.0.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    pacer.0 = Instant::now();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn culling_can_toggle_with_msaa_and_render_scale_changes() {
        let mut app = App::new();
        let mut images = Assets::<Image>::default();
        let target = images.add(Image::new_target_texture(
            1280,
            800,
            TextureFormat::Rgba8UnormSrgb,
            None,
        ));
        app.insert_resource(SceneTarget(target.clone()))
            .insert_resource(images)
            .insert_resource(Menu {
                // A fixed size, so the internal target follows a known window.
                open: false, selected: 0, settings: GraphicsSettings { width: 1280, height: 800, ..GraphicsSettings::default() },
                difficulty: Difficulty::Easy, custom:Tuning::default(), custom_defaults:Tuning::default(), custom_apply:false, custom_dirty:false, path: PathBuf::new(), status: String::new(),
                multiplayer: false, browser: false, network_page: 0, browser_count: 0, daylight: false, section: 0, custom_sections: Vec::new(), map_detail: false, destinations: Vec::new(), pending_travel: None, crossbar: false, controls_help: false, trick_guide: false, mode_pick: None, pending_online: None, injected: Vec::new(), front_end: false, front_world: 0, launch: false, hidden: false,
                maps: vec![crate::map_library::Entry { label: "Test world".into(), path: None }], selected_map: 0,
            })
            .add_systems(Update, apply);
        {
            let mut menu = app.world_mut().resource_mut::<Menu>();
            menu.settings.hour = 23.5;
            menu.settings.day_speed = 60;
            assert!((menu.advance_day(60.) - 0.5).abs() < 0.0001);
            menu.open = true;
            assert_eq!(menu.advance_day(60.), 0.5);
            menu.open = false;
            menu.settings.day_speed = 0;
            assert_eq!(menu.advance_day(60.), 0.5);
        }
        app.world_mut().spawn((Window::default(), PrimaryWindow));
        let camera = app.world_mut().spawn((Camera3d::default(), Msaa::Off)).id();
        app.update();
        // MSAA and occlusion culling are no longer settings, so the only thing
        // `apply` still adapts is the internal render target size.
        {
            let mut menu = app.world_mut().resource_mut::<Menu>();
            menu.settings.scale = 67;
        }
        app.update();
        assert_eq!(*app.world().get::<Msaa>(camera).unwrap(), Msaa::Off);
        assert_eq!(
            app.world()
                .resource::<Assets<Image>>()
                .get(&target)
                .unwrap()
                .size(),
            UVec2::new(857, 536)
        );
    }
    #[test]
    fn sections_expose_only_real_rows_and_all_maps() {
        let mut menu = Menu {
            open: true, selected: 1000, settings: GraphicsSettings::default(),
            path: PathBuf::new(), difficulty: Difficulty::Easy, custom:Tuning::default(), custom_defaults:Tuning::default(), custom_apply:false, custom_dirty:false, status: String::new(),
            maps: (0..40).map(|i| crate::map_library::Entry { label: format!("Map {i}"), path: None }).collect(),
            selected_map: 0, multiplayer: false, browser: false, network_page: 0, browser_count: 0, daylight: false, section: 0, custom_sections: Vec::new(), map_detail: false, destinations: Vec::new(), pending_travel: None, crossbar: false, controls_help: false, trick_guide: false, mode_pick: None, pending_online: None, injected: Vec::new(), front_end: false, front_world: 0, launch: false, hidden: false,
        };
        for section in 0..SECTIONS.len() {
            menu.select_section(section);
            let rows = menu.rows();
            assert!(rows.contains(&menu.selected));
            assert!(rows.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(rows.iter().all(|id| *id < 16 || *id >= 1000));
        }
        menu.select_section(1);
        assert_eq!(menu.rows(),vec![3,8,10]);
        menu.custom_sections=vec![("Challenges".into(),vec![("test.mod".into(),"race".into(),"Race".into())])];
        menu.select_section(SECTIONS.len());
        assert_eq!(menu.rows(),vec![200]);
        menu.select_section(0);
        assert_eq!(menu.rows(), (1000..1040).collect::<Vec<_>>());
        menu.maps[0].path = Some(PathBuf::from("University.skate"));
        menu.destinations = vec![
            crate::teleport_menu::Destination { id: "uni".into(), name: "Campus".into(), map: "University".into(), matrix: Some([[0.;4];4]) },
            crate::teleport_menu::Destination { id: "dt".into(), name: "Downtown".into(), map: "DownTown".into(), matrix: Some([[0.;4];4]) },
        ];
        menu.map_detail = true;
        assert_eq!(menu.rows(), vec![50,51,1_000_000]);
        menu.select_section(0);
        menu.maps.clear();
        menu.select_section(0);
        assert_eq!(menu.selected, 100);
        assert!(menu.rows().is_empty());
        menu.select_section(3);
        assert!(menu.multiplayer);
        assert_eq!(menu.rows(), vec![2,6,13,14,15]);
        menu.network_page = 4;
        assert_eq!(menu.rows(), (20..27).chain([10]).collect::<Vec<_>>());
        menu.network_page = 1;
        assert_eq!(menu.rows(), vec![3,4,10]);
        menu.network_page = 3;
        assert_eq!(menu.rows(), vec![0,1,10]);
        menu.browser = true;
        assert!(menu.rows().contains(&8));
        assert!(!SECTIONS.iter().any(|(name,_)| matches!(*name,"SESSION"|"WORLD")));
        menu.select_section(2);
        assert!(!menu.multiplayer && !menu.browser);
        assert_eq!(menu.rows(), vec![0,1,2,4,13]);
        menu.daylight = true;
        assert_eq!(menu.rows(), vec![0,1,2,3]);
    }
    #[test]
    fn invalid_saved_values_fall_back() {
        let settings: GraphicsSettings =
            serde_json::from_str(r#"{"width":0,"height":999999,"scale":0,"fps":1}"#)
                .unwrap();
        assert_eq!(settings.validated(), GraphicsSettings::default());
    }
    #[test]
    fn browser_defaults_are_conservative_and_keep_edge_balanced() {
        let firefox = GraphicsSettings::for_browser(crate::config::BrowserProfile::Firefox);
        assert_eq!((firefox.texture_quality, firefox.shadows, firefox.scale, firefox.fps), (1, 1, 85, 60));

        let safari = GraphicsSettings::for_browser(crate::config::BrowserProfile::Safari);
        assert_eq!((safari.texture_quality, safari.shadows, safari.scale, safari.fps), (2, 0, 75, 60));

        let edge = GraphicsSettings::for_browser(crate::config::BrowserProfile::Edge);
        assert_eq!(edge, GraphicsSettings::default());
    }
    #[test]
    fn performance_preset_marks_custom_and_builtin_settings() {
        let balanced = GraphicsSettings {
            texture_quality: 0,
            shadows: 2,
            anti_aliasing: 0,
            scale: 100,
            fps: 0,
            vsync: false,
            ..GraphicsSettings::default()
        };
        assert_eq!(balanced.performance_preset(), 1);
        let low_end = GraphicsSettings {
            texture_quality: 3,
            shadows: 0,
            anti_aliasing: 0,
            scale: 50,
            fps: 60,
            vsync: false,
            ..GraphicsSettings::default()
        };
        assert_eq!(low_end.performance_preset(), 2);
        assert_eq!(
            GraphicsSettings { scale: 75, ..balanced }.performance_preset(),
            0
        );
    }
    #[test]
    fn scaled_target_and_cycle_boundaries() {
        let s = GraphicsSettings {
            scale: 50,
            ..default()
        };
        assert_eq!(
            s.internal_size(UVec2::new(1920, 1080)),
            UVec2::new(960, 540)
        );
        assert_eq!(s.internal_size(UVec2::ZERO), UVec2::ONE);
        assert_eq!(cycle(LIMITS, 0, 1), 30);
        assert_eq!(cycle(LIMITS, 30, -1), 0);
        assert_eq!(cycle(LIMITS, 240, 1), 360);
        // An old saved cap snaps to the nearest one offered.
        let old = GraphicsSettings { fps: 90, ..GraphicsSettings::default() }.validated();
        assert_eq!(old.fps, 100);
    }
}

#[derive(Component)] struct CustomTrack(usize);
#[derive(Component)] struct CustomFill(usize);
fn custom_sliders(mut menu: ResMut<Menu>, mouse: Res<ButtonInput<MouseButton>>,
    tracks: Query<(&CustomTrack,&RelativeCursorPosition)>, mut fills: Query<(&CustomFill,&mut Node)>,
    mut dragging: Local<Option<usize>>) {
    if !menu.open || menu.section != 1 || menu.difficulty != Difficulty::Custom { *dragging=None; return; }
    if mouse.just_pressed(MouseButton::Left) {
        *dragging=tracks.iter().find(|(_,p)|p.cursor_over()).map(|(t,_)|t.0);
    }
    if !mouse.pressed(MouseButton::Left) { *dragging=None; }
    if let Some(index)=*dragging {
        if let Some((_,position))=tracks.iter().find(|(t,_)|t.0==index) {
            if let Some(pos)=position.normalized {
                menu.custom.set_fraction(index,pos.x); menu.custom_dirty=true; menu.selected=300+index;
            }
        }
    }
    for (fill,mut node) in &mut fills {node.width=percent(menu.custom.value(fill.0)/OPTIONS[fill.0].max*100.);}
}
fn apply_custom_difficulty(mut menu: ResMut<Menu>, config: Res<crate::config::Config>,
    mut skater: ResMut<crate::physics::SkaterRuntime>) {
    if !std::mem::take(&mut menu.custom_apply) {return;}
    let result=(|| -> Result<(),String> {
        let mut data=skate_data::collections::Collections::load(&config.asset_root)?;
        menu.custom.overlay(&mut data)?;
        skater.reload_difficulty(&data)?;
        menu.custom.save(&config.asset_root)?;
        Ok(())
    })();
    menu.status=match result {
        Ok(())=> {menu.custom_dirty=false;"Custom tuning applied and saved.".into()},
        Err(e)=>format!("Custom tuning: {e}"),
    };
}
