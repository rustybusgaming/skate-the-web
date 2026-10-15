//! Mirrors the staged startup in `main.rs`, fed from the VFS instead of the
//! process environment.
use super::host;
use crate::{
    app, camera,
    config::Config,
    difficulty::Difficulty,
    graph_runtime::StockGraphs,
    physics::{self, GamePhysics, SkaterRuntime},
    skate_world,
};
use skate_data::{GameAssets, skate_map::SkateMap, vfs};
use std::path::PathBuf;

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for b in bytes {
        hash = (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    hash
}

/// `?capture=1` renders one frame to a PNG in browser storage. The embedded
/// browser cannot capture a WebGPU canvas, and a readback through the engine's
/// own render target is the only way to see what the GPU actually produced.
mod capture {
    use bevy::{
        prelude::*,
        render::view::screenshot::{Screenshot, ScreenshotCaptured},
    };

    #[derive(Resource, Default)]
    struct Pending(f32);

    pub(super) fn install(app: &mut App) {
        app.init_resource::<Pending>()
            .add_systems(Update, request);
    }

    fn request(mut commands: Commands, time: Res<Time>, mut state: ResMut<Pending>) {
        state.0 += time.delta_secs();
        if state.0 > 12.0 {
            state.0 = f32::NEG_INFINITY;
            commands.spawn(Screenshot::primary_window()).observe(store);
        }
    }

    fn store(event: On<ScreenshotCaptured>) {
        let path = std::path::PathBuf::from("captures/frame.png");
        let size = event.image.texture_descriptor.size;
        let Some(data) = event.image.data.clone() else {
            bevy::log::error!("CAPTURE: screenshot has no CPU pixels");
            return;
        };
        let mut encoded = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut encoded);
        if let Err(error) = image::ImageEncoder::write_image(
            encoder,
            &data,
            size.width,
            size.height,
            image::ExtendedColorType::Rgba8,
        ) {
            bevy::log::error!("CAPTURE: encode failed: {error}");
            return;
        }
        match skate_data::vfs::write(&path, &encoded) {
            Ok(()) => bevy::log::info!("CAPTURE_READY {}x{} bytes={}", size.width, size.height, encoded.len()),
            Err(error) => bevy::log::error!("CAPTURE: write failed: {error}"),
        }
    }

}

/// The page keeps its loading screen up until the engine has run a frame.
mod ready {
    use bevy::prelude::*;

    pub(super) fn install(app: &mut App) {
        app.add_systems(Update, announce);
    }

    fn announce(mut announced: Local<bool>) {
        if *announced {
            return;
        }
        *announced = true;
        super::host::ready();
    }
}

pub(super) async fn boot() -> Result<(), String> {
    vfs::set_write_hook(host::persist);
    // ?trace-reads: log every file the engine opens (finding unused data).
    if web_sys::window().and_then(|w| w.location().search().ok()).is_some_and(|q| q.contains("trace-reads")) {
        vfs::trace_reads(true);
    }
    let boot = host::boot_config().await?;
    host::stage("Mounting game data…");
    let total = boot.mount.len() as u32;
    for (index, path) in boot.mount.iter().enumerate() {
        host::progress(index as u32, total, "Mounting game data");
        let bytes = host::read(path).await?.ok_or_else(|| format!("{path}: missing from browser storage"))?;
        vfs::mount(path, bytes);
    }
    host::progress(total, total, "Mounted game data");
    // Files the page holds but did not mount (the maps, other worlds' skies):
    // listed, and fetched when a world loads.
    for path in &boot.declared {
        vfs::declare(path);
    }
    // Every startup file now lives in the engine; the page's copy of the
    // package is a second ~300 MB that a phone cannot spare while the map loads.
    host::release_boot();
    host::stage("Reading the map…");
    let asset_root = PathBuf::from(&boot.root);
    let difficulty = match &boot.difficulty {
        Some(value) => Difficulty::parse(value)?,
        None => Difficulty::load(&asset_root)?,
    };
    let (map, map_path, map_fingerprint) = match &boot.map {
        Some(path) => {
            let bytes = match vfs::read(path) {
                Ok(bytes) => bytes,
                Err(_) => host::read(path).await?.ok_or_else(|| format!("{path}: map missing from browser storage"))?,
            };
            let hash = fingerprint(&bytes);
            // Pixels stay compressed until the renderer packs them one texture
            // at a time; decoding a city up front costs about a gigabyte of a
            // heap that never shrinks.
            let map = SkateMap::parse_deferred(bytes)?;
            // The skater and board are lit from the map's authored light probes,
            // stored beside it. Maps load on demand rather than at boot, so
            // fetch this one with its map; without it the character renders
            // with no sun, probes or shadow casters.
            let probes = std::path::Path::new(path).with_extension("irradiance");
            let probes = probes.to_string_lossy().replace('\\', "/");
            if vfs::read(&probes).is_err() {
                match host::read(&probes).await {
                    Ok(Some(bytes)) => vfs::mount(&probes, bytes),
                    Ok(None) => bevy::log::warn!("{probes}: not found; the skater will be unlit"),
                    Err(error) => bevy::log::warn!("{probes}: {error}"),
                }
            }
            (Some(map), Some(PathBuf::from(path)), hash)
        }
        None => (None, None, 0),
    };
    let config = Config {
        asset_root: asset_root.clone(),
        verification_capture: None,
        map,
        map_path,
        difficulty,
        check_assets: false,
        start_paused: false,
        multiplayer: crate::multiplayer::Options::default(),
        map_fingerprint,
        teleport: boot.teleport.clone(),
        front_end: boot.front_end,
        mobile: boot.mobile,
        browser: boot.browser,
    };
    skate_data::input_config::StockGameplayConfig::load(&asset_root).map_err(|e| e.to_string())?;
    host::stage("Reading game data…");
    let manifest = GameAssets::load(&asset_root).map_err(|e| e.to_string())?;
    let graphs = StockGraphs::load(&asset_root, &manifest)?;
    if let Some(map) = &config.map {
        skate_world::validate_runtime(map)?;
    }
    host::stage("Loading physics…");
    let physics = GamePhysics::load_with_difficulty(&asset_root, config.map.as_ref(), config.difficulty)?;
    let skater = SkaterRuntime::load(&asset_root, &graphs, &physics, config.difficulty.profile_key())?;
    let controls = physics::PlayerControls::load(&asset_root)?;
    camera::CameraRuntime::load(&asset_root)?;
    host::stage("Building the world…");
    let mut app = app::build(config, manifest, graphs, physics, skater);
    app.insert_resource(controls);
    host::stage("Starting the renderer…");
    ready::install(&mut app);
    if boot.capture {
        capture::install(&mut app);
    }
    app.run();
    Ok(())
}
