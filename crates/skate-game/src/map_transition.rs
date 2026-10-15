//! Transactional world replacement: prepare without mutating the live session,
//! then commit at a schedule boundary with gameplay suspended.
use bevy::prelude::*;
use web_time::{Instant};
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicU8, Ordering}}, thread::JoinHandle};
use crate::{config::Config, map_library::Entry, map_render::PreparedScene,
    physics::{GamePhysics, PlayerControls, SkaterRuntime}};

/// Persistent character customisation can reapply after this set, while the
/// transition still holds the loading overlay and gameplay remains suspended.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MapTransitionSet;
#[derive(Message)]
pub(crate) struct WorldChanged;


#[cfg(test)]
#[path = "tests/map_transition.rs"]
mod tests;

#[derive(Resource, Debug)]
pub(crate) struct CurrentMap {
    pub path: Option<PathBuf>,
    pub name: String,
    pub spawn: [f32; 3],
    pub heading: f32,
    pub generation: u64,
}
impl CurrentMap {
    fn from_package(path: Option<PathBuf>, map: Option<&skate_data::skate_map::SkateMap>) -> Self {
        Self { path, name: map.map_or_else(|| "Test world".into(), |m| m.name.clone()),
            spawn: map.map_or([0., crate::physics::ground::HEIGHT, 0.], |m| m.spawn),
            heading: map.map_or(0., |m| m.heading), generation: 0 }
    }
}

struct PreparedWorld {
    map_fingerprint: u64,
    scene: PreparedScene,
    physics: GamePhysics,
    skater: SkaterRuntime,
    controls: PlayerControls,
    camera: crate::camera::CameraRuntime,
    metadata: CurrentMap,
    retail: bool,
    difficulty: crate::difficulty::Difficulty,
}

/// The world being built: on a worker thread natively, or already finished on
/// the web, which has no threads and builds it on the main thread.
enum Job {
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Thread(JoinHandle<Result<PreparedWorld, String>>),
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Done(Result<PreparedWorld, String>),
}
impl Job {
    fn is_finished(&self) -> bool {
        match self {
            Job::Thread(job) => job.is_finished(),
            Job::Done(_) => true,
        }
    }
    fn join(self) -> Result<PreparedWorld, String> {
        match self {
            Job::Thread(job) => job.join().unwrap_or_else(|_| Err("Map loader failed unexpectedly".into())),
            Job::Done(result) => result,
        }
    }
}
/// A web fetch of the map's files: `None` until it settles.
type Fetch = Arc<std::sync::Mutex<Option<Result<(), String>>>>;

enum Phase {
    Idle,
    Requested(Entry),
    /// Web: the map and its light probes are being downloaded.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Fetching { entry: Entry, fetch: Fetch },
    /// Web: downloaded; built a couple of frames later, after the loading
    /// screen has drawn.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    Building { entry: Entry, frames: u8 },
    Loading { entry: Entry, progress: Arc<AtomicU8>, job: Job },
    // Let extraction see the committed scene before releasing the pause menu.
    Publishing { frames: u8, notice: String },
}
#[derive(Resource)]
pub(crate) struct MapTransition { phase: Phase }
impl Default for MapTransition {
    fn default() -> Self { Self { phase: Phase::Idle } }
}
impl MapTransition {
    pub fn busy(&self) -> bool { !matches!(self.phase, Phase::Idle) }
    pub fn request(&mut self, entry: Entry) {
        if !self.busy() { self.phase = Phase::Requested(entry); }
    }
    pub fn label(&self) -> String {
        match &self.phase {
            Phase::Idle => String::new(),
            Phase::Requested(entry) | Phase::Building { entry, .. } => format!("Loading {} — preparing…", entry.label),
            Phase::Fetching { entry, .. } => format!("Loading {} — {}…", entry.label, download(entry).to_lowercase()),
            Phase::Loading { entry, progress, .. } => format!("Loading {} — {}…", entry.label,
                match progress.load(Ordering::Relaxed) {
                    0 => "reading and validating map", 1 => "building collision and rendering",
                    2 => "initializing skater, camera and rendering", _ => "finishing meshes, textures and sky",
                }),
            Phase::Publishing { .. } => "Loading — publishing the new world…".into(),
        }
    }
}

/// How a world's download is going, e.g. "DOWNLOADING 120 / 360 MB".
fn download(entry: &Entry) -> String {
    #[cfg(target_arch = "wasm32")]
    if let Some((done, total, idle)) = entry.path.as_ref().and_then(|p| crate::web::download_status(&p.to_string_lossy().replace('\\', "/"))) {
        let mb = |b: f64| (b / 1048576.).round();
        let amount = if total > 0. { format!("{} / {} MB", mb(done), mb(total)) } else { format!("{} MB", mb(done)) };
        // Nothing for a while: the page retries by itself; say so.
        return if idle > 20. { format!("DOWNLOADING {amount} · WAITING FOR DATA, RETRYING") } else { format!("DOWNLOADING {amount}") };
    }
    let _ = entry;
    "DOWNLOADING".into()
}

impl MapTransition {
    /// A few words for Skate 3's own loading screen: what the load is doing.
    pub fn status(&self) -> String {
        match &self.phase {
            Phase::Idle => String::new(),
            Phase::Requested(_) | Phase::Building { .. } => "PREPARING THE WORLD".into(),
            Phase::Fetching { entry, .. } => download(entry),
            Phase::Loading { progress, .. } => match progress.load(Ordering::Relaxed) {
                0 => "READING THE MAP",
                1 => "BUILDING THE WORLD",
                2 => "SETTING UP YOUR SKATER",
                _ => "FINISHING TEXTURES",
            }
            .into(),
            Phase::Publishing { .. } => "ALMOST THERE".into(),
        }
    }
}

pub(crate) struct MapTransitionPlugin;
impl Plugin for MapTransitionPlugin {
    fn build(&self, app: &mut App) {
        let config = app.world().resource::<Config>();
        let current = CurrentMap::from_package(config.map_path.clone(), config.map.as_ref());
        app.insert_resource(current)
            .init_resource::<MapTransition>()
            .add_message::<WorldChanged>()
            .add_systems(PreUpdate, poll.in_set(MapTransitionSet).after(crate::graphics_menu::MenuInput)
                .before(crate::input::poll_controllers))
            .configure_sets(FixedUpdate, (
                crate::app::SimulationSet::Input, crate::app::SimulationSet::Controls,
                crate::app::SimulationSet::Physics,
            ).run_if(crate::graphics_menu::gameplay_active));
    }
}

/// What a world build needs from the live session, taken before it starts.
struct Inputs {
    root: PathBuf,
    difficulty: crate::difficulty::Difficulty,
    graphs: crate::graph_runtime::StockGraphs,
    source: Arc<crate::skater_animation::AnimationSource>,
    preferences: skate_core::input::riding_intentions::PushPreferences,
    scene: PreparedScene,
    selected: Entry,
}
fn inputs(world: &World, entry: Entry) -> Inputs {
    let config = world.resource::<Config>();
    Inputs {
        root: config.asset_root.clone(),
        difficulty: config.difficulty,
        graphs: world.resource::<crate::graph_runtime::StockGraphs>().clone(),
        source: world.resource::<SkaterRuntime>().animation.source.clone(),
        preferences: world.resource::<PlayerControls>().preferences,
        scene: PreparedScene::new(world),
        selected: entry,
    }
}

/// Build the new world without touching the live one.
fn prepare(inputs: Inputs, stage: Arc<AtomicU8>) -> Result<PreparedWorld, String> {
    let Inputs { root, difficulty, graphs, source, preferences, mut scene, selected } = inputs;
    let _span = info_span!("load_map_transition").entered();
    let started = Instant::now();
    let read = info_span!("read_map").in_scope(|| selected.path.as_deref().map(read_map).transpose())?;
    let (map, map_fingerprint) = match read {
        Some((map, fingerprint)) => (Some(map), fingerprint),
        None => (None, crate::config::map_fingerprint(None)?),
    };
    let read_time = started.elapsed();
    let validation_started = Instant::now();
    if let Some(map) = &map { crate::skate_world::validate_runtime(map)?; }
    let validation_time = validation_started.elapsed();
    let metadata = CurrentMap::from_package(selected.path, map.as_ref());
    let retail = map.as_ref().is_some_and(|m| crate::retail_render::RetailScene::for_map(m));
    #[cfg(target_arch = "wasm32")]
    let mut map = map;

    let simulate = |stage: &AtomicU8| -> Result<_, String> {
        stage.store(1, Ordering::Relaxed);
        let physics = info_span!("load_physics").in_scope(|| GamePhysics::load_with_difficulty(&root, map.as_ref(), difficulty))?;
        stage.store(2, Ordering::Relaxed);
        let skater = SkaterRuntime::load_for_world(&root, &graphs, &physics, difficulty.profile_key(), Some(source))?;
        let mut controls = PlayerControls::load(&root)?;
        controls.preferences = preferences;
        let camera = crate::camera::CameraRuntime::load(&root)?;
        Ok((physics, skater, controls, camera))
    };
    // Natively both builders only read the decoded package: render handles
    // are reserved on a second worker while the first constructs fresh
    // simulation state. Neither publishes until both have succeeded.
    #[cfg(not(target_arch = "wasm32"))]
    let (physics, skater, controls, camera, simulation_time, render_time) =
        std::thread::scope(|scope| -> Result<_, String> {
            let rendering = std::thread::Builder::new().name("map-render-loader".into())
                .spawn_scoped(scope, || {
                    let render_started = Instant::now();
                    let pixels: Option<crate::retail_render::TexturePixels<'static>> =
                        map.as_mut().map(crate::retail_render::TexturePixels::taken);
                    scene.prepare(map.as_ref(), pixels, &root);
                    render_started.elapsed()
                }).map_err(|e| format!("Could not start render loader: {e}"))?;
            let simulation_started = Instant::now();
            let simulation = simulate(&stage);
            let simulation_time = simulation_started.elapsed();
            stage.store(3, Ordering::Relaxed);
            // Explicitly join even on a simulation error: no background
            // render work or reserved scene can outlive a failed request.
            let render_time = rendering.join()
                .map_err(|_| "Map render preparation failed unexpectedly".to_string())?;
            let (physics, skater, controls, camera) = simulation?;
            Ok((physics, skater, controls, camera, simulation_time, render_time))
        })?;
    // The web has no threads: the same work, one after the other.
    #[cfg(target_arch = "wasm32")]
    let (physics, skater, controls, camera, simulation_time, render_time) = {
        let simulation_started = Instant::now();
        let (physics, skater, controls, camera) = simulate(&stage)?;
        let simulation_time = simulation_started.elapsed();
        stage.store(3, Ordering::Relaxed);
        let render_started = Instant::now();
        scene.prepare(map.as_ref(), None, &root);
        (physics, skater, controls, camera, simulation_time, render_started.elapsed())
    };
    eprintln!("MAP_LOAD_TIMING name={:?} read_ms={} validation_ms={} simulation_ms={} render_ms={} prepare_ms={}",
        metadata.name, read_time.as_millis(), validation_time.as_millis(),
        simulation_time.as_millis(), render_time.as_millis(), started.elapsed().as_millis());
    // Drop the decoded package here. Physics and rendering now own their
    // data; retaining it would double large-city CPU memory.
    Ok(PreparedWorld { map_fingerprint, scene, physics, skater, controls, camera, metadata, retail, difficulty })
}

/// Natively the whole file is decoded at once; the web keeps texture pixels
/// compressed until the renderer packs them (a city decoded up front costs
/// about a gigabyte of a heap that never shrinks).
/// Also the map's fingerprint, from the same bytes on the web (one copy of a
/// city is all a phone can spare).
fn read_map(path: &std::path::Path) -> Result<(skate_data::skate_map::SkateMap, u64), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let bytes = skate_data::vfs::take(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let fingerprint = bytes.iter().fold(0xcbf29ce484222325u64, |hash, b| (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3));
        Ok((skate_data::skate_map::SkateMap::parse_deferred(bytes)?, fingerprint))
    }
    #[cfg(not(target_arch = "wasm32"))]
    Ok((skate_data::skate_map::SkateMap::load(path)?, crate::config::map_fingerprint(Some(path))?))
}

#[cfg(not(target_arch = "wasm32"))]
fn start(world: &World, entry: Entry) -> Result<Phase, String> {
    let inputs = inputs(world, entry.clone());
    let progress = Arc::new(AtomicU8::new(0));
    let stage = progress.clone();
    let job = std::thread::Builder::new().name("map-loader".into())
        .spawn(move || prepare(inputs, stage))
        .map_err(|e| format!("Could not start map loader: {e}"))?;
    Ok(Phase::Loading { entry, progress, job: Job::Thread(job) })
}

/// The web fetches the map (and its light probes) from the page first.
#[cfg(target_arch = "wasm32")]
fn start(_world: &World, entry: Entry) -> Result<Phase, String> {
    let fetch: Fetch = Default::default();
    let path = entry.path.as_ref().map(|p| p.to_string_lossy().replace('\\', "/"));
    let settled = fetch.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let result: Result<(), String> = async {
            let Some(path) = path else { return Ok(()) };
            // Checked without reading: a read would copy the whole city.
            if skate_data::vfs::needs_fetch(&path) || !skate_data::vfs::is_file(&path) {
                let bytes = crate::web::host_read(&path).await?.ok_or_else(|| format!("{path}: not in the game data"))?;
                skate_data::vfs::mount(&path, bytes);
            }
            // The world's sky: the page mounts only the starting map's.
            let stem = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            for suffix in [".json", ".rgba", ".sun.rgba"] {
                let sky = format!("assets/private/native-skies/{stem}{suffix}");
                if skate_data::vfs::needs_fetch(&sky) {
                    if let Some(bytes) = crate::web::host_read(&sky).await? {
                        skate_data::vfs::mount(&sky, bytes);
                    }
                }
            }
            let probes = std::path::Path::new(&path).with_extension("irradiance").to_string_lossy().replace('\\', "/");
            if skate_data::vfs::read(&probes).is_err() {
                match crate::web::host_read(&probes).await {
                    Ok(Some(bytes)) => skate_data::vfs::mount(&probes, bytes),
                    Ok(None) => warn!("{probes}: not found; the skater will be unlit"),
                    Err(error) => warn!("{probes}: {error}"),
                }
            }
            Ok(())
        }
        .await;
        if let Ok(mut settled) = settled.lock() {
            *settled = Some(result);
        }
    });
    Ok(Phase::Fetching { entry, fetch })
}

fn poll(world: &mut World) {
    let ready = match &world.resource::<MapTransition>().phase {
        Phase::Idle => false,
        Phase::Loading { job, .. } => job.is_finished(),
        Phase::Fetching { fetch, .. } => fetch.lock().is_ok_and(|f| f.is_some()),
        _ => true,
    };
    if !ready { return; }
    let phase = std::mem::replace(&mut world.resource_mut::<MapTransition>().phase, Phase::Idle);
    let next = match phase {
        Phase::Requested(entry) => match start(world, entry) {
            Ok(phase) => phase,
            Err(error) => { failed(world, error); Phase::Idle }
        },
        Phase::Fetching { entry, fetch } => match fetch.lock().ok().and_then(|mut f| f.take()).unwrap_or(Ok(())) {
            Ok(()) => Phase::Building { entry, frames: 2 },
            Err(error) => { failed(world, error); Phase::Idle }
        },
        // Give the loading screen a frame or two to draw before the main
        // thread is busy building.
        Phase::Building { entry, frames } if frames > 0 => Phase::Building { entry, frames: frames - 1 },
        Phase::Building { entry, .. } => {
            let progress = Arc::new(AtomicU8::new(0));
            let result = prepare(inputs(world, entry.clone()), progress.clone());
            Phase::Loading { entry, progress, job: Job::Done(result) }
        }
        Phase::Loading { job, .. } => match job.join() {
            Ok(prepared) => {
                let mut notice = commit(world, prepared);
                let config = world.resource::<Config>();
                match crate::map_library::save_default(&config.asset_root, config.map_path.as_deref()) {
                    Ok(()) => notice.push_str(" Default map saved."),
                    Err(error) => notice.push_str(&format!(" Could not save default: {error}")),
                }
                Phase::Publishing { frames: 3, notice }
            }
            Err(error) => { failed(world, error); Phase::Idle }
        },
        Phase::Publishing { frames, notice } if frames > 0 => Phase::Publishing { frames: frames - 1, notice },
        Phase::Publishing { notice, .. } => {
            world.resource_mut::<crate::graphics_menu::Menu>().transition_finished(notice, true);
            world.resource_mut::<Time<Virtual>>().unpause();
            Phase::Idle
        }
        Phase::Idle => Phase::Idle,
    };
    world.resource_mut::<MapTransition>().phase = next;
}

fn failed(world: &mut World, error: String) {
    warn!("MAP_TRANSITION_FAILED {error}");
    world.resource_mut::<crate::graphics_menu::Menu>()
        .transition_finished(format!("Could not load map: {error}\nPrevious world retained. Choose another map or Resume."), false);
}

fn commit(world: &mut World, mut prepared: PreparedWorld) -> String {
    let publication_started = Instant::now();
    prepared.metadata.generation = world.resource::<CurrentMap>().generation + 1;
    // All fallible decoding/validation/construction finished before this point.
    // No simulation system can observe a mixture of the two worlds.
    crate::map_render::MapAssets::retire(world);
    prepared.scene.publish(world);
    crate::camera::set_world_environment(world, prepared.retail);
    world.insert_resource(crate::retail_render::RetailScene(prepared.retail));
    world.insert_resource(crate::grind_world::GrindGeometry::for_world(prepared.metadata.path.is_none()));
    world.insert_resource(Time::<Fixed>::from_duration(prepared.physics.period()));
    let root_transform = Transform::from_matrix(crate::animation::native_matrix(
        prepared.skater.animated_skeleton.roots.animation_to_world));
    for mut transform in world.query_filtered::<&mut Transform, With<crate::world::PlayerRoot>>().iter_mut(world) {
        *transform = root_transform;
    }
    world.insert_resource(prepared.physics);
    world.insert_resource(prepared.skater);
    world.insert_resource(prepared.controls);
    world.insert_resource(prepared.camera);
    world.insert_resource(crate::presentation::Presentation::default());
    world.insert_resource(crate::replay::Replay::default());
    world.insert_resource(crate::input::ControllerInput::default());
    world.insert_resource(crate::input::PublishedTickInput::default());
    world.resource_mut::<ButtonInput<KeyCode>>().reset_all();
    world.resource_mut::<ButtonInput<MouseButton>>().reset_all();
    let mut config = world.resource_mut::<Config>();
    config.map_path = prepared.metadata.path.clone();
    config.map_fingerprint = prepared.map_fingerprint;
    config.difficulty = prepared.difficulty;
    let notice = format!("Loaded {}.", prepared.metadata.name);
    info!("MAP_TRANSITION_COMMITTED generation={} name={:?} spawn={:?} heading={} pid={}",
        prepared.metadata.generation, prepared.metadata.name, prepared.metadata.spawn,
        prepared.metadata.heading, crate::platform::process_id());
    if let Some(mut messages) = world.get_resource_mut::<Messages<WorldChanged>>() {
        messages.write(WorldChanged);
    }
    world.insert_resource(prepared.metadata);
    eprintln!("MAP_PUBLISH_TIMING cpu_ms={}", publication_started.elapsed().as_millis());
    notice
}
