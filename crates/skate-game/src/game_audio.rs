//! Gameplay sound, from the player's own converted Skate 3 audio.
//!
//! `tools/asset_pipeline/audio.py` turns the disc's EA banks into Ogg Vorbis
//! under `assets/private/audio/` and writes `sounds.json`: gameplay events
//! (pop, land, grind start, bail...) to clips, and looping beds (rolling,
//! grinding, wind...) to one clip each with speed-driven level and pitch. This
//! module reads that table and follows the physics' published state and events
//! every fixed tick, so the sound matches what the simulation did rather than
//! what the animation shows.
//!
//! Without converted audio the table is missing and the game stays silent.
use bevy::{
    audio::{AudioPlayer, AudioSink, AudioSinkPlayback, AudioSource, PlaybackSettings, Volume},
    prelude::*,
};
use serde::Deserialize;
use skate_core::physics::phase::PhysicsEvent;
use skate_core::player::state::PhysicalStateId as State;
use std::collections::HashMap;

#[derive(Deserialize)]
struct Table {
    version: u32,
    #[serde(default)]
    events: HashMap<String, EventSpec>,
    #[serde(default)]
    loops: HashMap<String, LoopSpec>,
    /// Each map's background bed, by map file name.
    #[serde(default)]
    maps: HashMap<String, String>,
    /// Rolling as Skate 3's grain player makes it: loops of the surface take
    /// at a few board speeds, crossfaded by speed.
    #[serde(default)]
    rolling: Option<RollingSpec>,
    /// Board impacts from the game's material table: the board parts' own
    /// layers and each audio surface's.
    #[serde(default)]
    impacts: Option<ImpactsSpec>,
    /// The rest of the board and the skater, from the game's banks and
    /// tables: grinds, skids, wind, rattles, flips, scrapes, steps, bails.
    #[serde(default)]
    gameplay: Option<GameplaySpec>,
}
#[derive(Deserialize, Clone)]
struct ImpactsSpec {
    parts: HashMap<String, Option<ImpactSpec>>,
    surfaces: Vec<Option<ImpactSpec>>,
    /// Hall of Meat: each body part's bone sounds.
    #[serde(default)]
    hom: HashMap<String, HomSpec>,
}
#[derive(Deserialize, Clone)]
struct HomSpec {
    cues: Vec<LayerSpec>,
    volume: f32,
}
#[derive(Deserialize, Clone, Default)]
struct GameplaySpec {
    #[serde(default)]
    skids: Option<SkidsSpec>,
    #[serde(default)]
    wind: Option<WindSpec>,
    #[serde(default)]
    rattles: Vec<Vec<String>>,
    #[serde(default)]
    flips: HashMap<String, Vec<String>>,
    #[serde(default)]
    scrapes: HashMap<String, Vec<String>>,
    #[serde(default)]
    squeaks: Vec<Vec<String>>,
    #[serde(default)]
    steps: Vec<Vec<String>>,
    #[serde(default)]
    bodyslide: Vec<Vec<String>>,
    #[serde(default)]
    cloth: HashMap<String, Vec<String>>,
    #[serde(default)]
    grinds: Option<GrindsSpec>,
}
#[derive(Deserialize, Clone)]
struct SkidsSpec {
    sets: Vec<Vec<String>>,
    surface: Vec<u32>,
}
#[derive(Deserialize, Clone)]
struct WindSpec {
    loops: Vec<String>,
    gusts: Vec<String>,
    speed: [f32; 2],
}
#[derive(Deserialize, Clone)]
struct GrindsSpec {
    types: Vec<Option<GrindSpec>>,
    default: Option<GrindSpec>,
    surface: Vec<u32>,
}
/// One grind type (the game's grind table row): enter and exit hits by
/// speed tier, the board-slide pair, the loop layers and their volumes.
#[derive(Deserialize, Clone)]
struct GrindSpec {
    name: String,
    enter: Vec<LayerSpec>,
    exit: Vec<LayerSpec>,
    slide_enter: Option<LayerSpec>,
    slide_exit: Option<LayerSpec>,
    enter_volume: [f32; 3],
    exit_volume: [f32; 3],
    rail_volume: [f32; 3],
    max_speed: f32,
    loops: Vec<Vec<String>>,
}
#[derive(Deserialize, Clone)]
struct ImpactSpec {
    hard: Vec<LayerSpec>,
    medium: Vec<LayerSpec>,
    soft: Vec<LayerSpec>,
    extra: Vec<LayerSpec>,
    volume: f32,
    jump: f32,
}
#[derive(Deserialize, Clone)]
struct LayerSpec {
    clips: Vec<String>,
    volume: f32,
}
#[derive(Deserialize, Clone)]
struct RollingSpec {
    bands: Vec<BandSpec>,
    volume: [f32; 2],
    speed: [f32; 2],
    /// Each surface take's bands, by grain name.
    #[serde(default)]
    surfaces: HashMap<String, Vec<BandSpec>>,
    /// The take for each audio surface code (AudioSurfaceMap index)...
    #[serde(default)]
    map: Vec<String>,
    /// ...its level...
    #[serde(default)]
    gain: Vec<f32>,
    /// ...and the distance rolled between seam clacks, [min, max] metres.
    #[serde(default)]
    seams: Vec<[f32; 2]>,
    /// The same surfaces' takes on soft wheels.
    #[serde(default)]
    map_soft: Vec<String>,
}
#[derive(Deserialize, Clone)]
struct BandSpec {
    clip: String,
    speed: f32,
}
#[derive(Deserialize, Clone)]
struct EventSpec {
    clips: Vec<String>,
    volume: [f32; 2],
    pitch: [f32; 2],
}
#[derive(Deserialize, Clone)]
struct LoopSpec {
    clip: Option<String>,
    volume: [f32; 2],
    pitch: [f32; 2],
    speed: [f32; 2],
}

struct Event {
    clips: Vec<Handle<AudioSource>>,
    volume: [f32; 2],
    pitch: [f32; 2],
}
struct Ambience {
    generation: u64,
    entity: Entity,
    track: Option<usize>,
}
/// One material's impact: layers by strength, each a cue with random variants.
struct Impact {
    tiers: [Vec<Layer>; 3],
    extra: Vec<Layer>,
    volume: f32,
    jump: f32,
}
struct Layer {
    clips: Vec<Handle<AudioSource>>,
    volume: f32,
}
#[derive(Clone, Copy)]
enum Strength {
    Hard = 0,
    Medium = 1,
    Soft = 2,
}
struct Impacts {
    board: Option<Impact>,
    truck: Option<Impact>,
    surfaces: Vec<Option<Impact>>,
    /// Body parts (arm, leg, torso, head, foot) and bone cracks.
    parts: HashMap<String, Impact>,
    hom: HashMap<String, (Vec<Layer>, f32)>,
}
type Clips = Vec<Handle<AudioSource>>;
struct Grind {
    enter: Vec<Layer>,
    exit: Vec<Layer>,
    slide_enter: Option<Layer>,
    slide_exit: Option<Layer>,
    enter_volume: [f32; 3],
    exit_volume: [f32; 3],
    rail_volume: [f32; 3],
    max_speed: f32,
    loops: Vec<Clips>,
}
struct Gameplay {
    skids: Vec<Clips>,
    skid_surface: Vec<u32>,
    wind: Clips,
    gusts: Clips,
    wind_speed: [f32; 2],
    rattles: Vec<Clips>,
    flips: HashMap<String, Clips>,
    scrapes: HashMap<String, Clips>,
    squeaks: Vec<Clips>,
    steps: Vec<Clips>,
    bodyslide: Vec<Clips>,
    cloth: HashMap<String, Clips>,
    grinds: Vec<Option<Grind>>,
    grind_default: Option<Grind>,
    grind_surface: Vec<u32>,
}
impl Gameplay {
    fn grind(&self, code: Option<u32>) -> Option<&Grind> {
        let by_surface = code
            .and_then(|c| self.grind_surface.get(c as usize))
            .and_then(|t| self.grinds.get(*t as usize))
            .and_then(|g| g.as_ref());
        by_surface.or(self.grind_default.as_ref())
    }
}

impl Impacts {
    /// The impact set of an audio surface.
    fn surface(&self, code: Option<u32>) -> Option<&Impact> {
        self.surfaces.get(code? as usize)?.as_ref()
    }
}

struct Bed {
    entity: Entity,
    spec: LoopSpec,
    /// Smoothed level, so beds swell and fade instead of switching.
    level: f32,
    target: f32,
    pitch: f32,
}

/// Sounds other systems ask for by name: engine events (stingers) or the
/// FE sound names the original screens request (`FE_SOUND_CORE_A_BUTTON`...).
#[derive(Resource, Default)]
pub(crate) struct SoundQueue(pub Vec<std::borrow::Cow<'static, str>>);

impl SoundQueue {
    pub fn push(&mut self, name: impl Into<std::borrow::Cow<'static, str>>) {
        self.0.push(name.into());
    }
}

/// For a table converted before the front end's own patches were rendered:
/// the closest engine event for an FE sound name.
fn fallback(name: &str) -> Option<&'static str> {
    Some(match name {
        "FE_SOUND_CORE_A_BUTTON" | "FE_SOUND_TITLESCREEN_START" | "FE_SOUND_CHALLENGE_COUNT_GO" => "menu_select",
        "FE_SOUND_CORE_B_BUTTON" => "menu_back",
        "FE_SOUND_CORE_NAV_UP" | "FE_SOUND_CORE_NAV_DOWN" | "FE_SOUND_CHALLENGE_COUNT_3" | "FE_SOUND_CHALLENGE_COUNT_2"
        | "FE_SOUND_CHALLENGE_COUNT_1" => "menu_move",
        "FE_SOUND_HOM_TRANSITION" => "body",
        _ => return None,
    })
}

#[derive(Resource)]
pub(crate) struct GameAudio {
    events: HashMap<String, Event>,
    beds: HashMap<&'static str, Bed>,
    random: u64,
    /// The player's Sound Volume, 0-1.
    master: f32,
    previous: Option<State>,
    air_fall: f32,
    seam_in: f32,
    step_in: f32,
    skid_in: f32,
    body_in: f32,
    trick_seq: u32,
    bail_seq: u32,
    /// Each rolling take's beds (spawned on first use), slowest first, with
    /// their speeds; "" is the default take.
    rolling: HashMap<String, Vec<(&'static str, f32)>>,
    rolling_spec: Option<RollingSpec>,
    /// The take under the wheels and its level.
    rolling_take: (String, f32),
    /// The seam spacing of the surface under the wheels.
    seam_spacing: [f32; 2],
    impacts: Option<std::sync::Arc<Impacts>>,
    gameplay: Option<std::sync::Arc<Gameplay>>,
    /// The audio surface last under the wheels (for a pop, which leaves it).
    last_surface: Option<u32>,
    grind_run: Option<(bool, Vec<&'static str>, usize)>,
    squeak_in: f32,
    gust_in: f32,
    whooshed: bool,
    flipped: bool,
    wheel_hardness: f32,
    /// The playing background bed and the map generation / playlist slot it uses.
    ambience: Option<Ambience>,
    playlist: Vec<(String, String)>,
    music_generation: Option<u64>,
    music_index: Option<usize>,
    music_paused: bool,
    #[cfg(target_arch = "wasm32")]
    reported_music: Option<(String, bool, bool)>,
}

impl GameAudio {
    fn random(&mut self) -> f32 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        (self.random >> 40) as f32 / (1u64 << 24) as f32
    }

    fn play(&mut self, commands: &mut Commands, name: &str, gain: f32) {
        let pick = self.random();
        let volume_roll = self.random();
        let pitch_roll = self.random();
        let Some(event) = self.events.get(name) else { return };
        if event.clips.is_empty() || self.master <= 0. {
            return;
        }
        let clip = event.clips[((pick * event.clips.len() as f32) as usize).min(event.clips.len() - 1)].clone();
        let volume = (event.volume[0] + (event.volume[1] - event.volume[0]) * volume_roll) * gain * self.master;
        let speed = event.pitch[0] + (event.pitch[1] - event.pitch[0]) * pitch_roll;
        commands.spawn((
            AudioPlayer::new(clip),
            PlaybackSettings { volume: Volume::Linear(volume), speed, ..PlaybackSettings::DESPAWN },
            Name::new("Sound"),
        ));
    }

    /// Rolling at `speed` on audio surface `surface`: that surface's take,
    /// the two bands either side of the speed crossfaded with equal power and
    /// nudged in pitch toward the exact speed. Other takes fade out.
    fn roll(&mut self, commands: &mut Commands, server: &AssetServer, amount: f32, speed: f32, surface: Option<u32>) {
        let Some(spec) = self.rolling_spec.clone() else {
            self.bed("roll", amount, speed);
            return;
        };
        if let Some(code) = surface {
            let soft = self.wheel_hardness < 0.5;
            let take = if soft { spec.map_soft.get(code as usize).or(spec.map.get(code as usize)) } else { spec.map.get(code as usize) };
            if let Some(take) = take {
                let gain = spec.gain.get(code as usize).copied().unwrap_or(1.);
                if self.rolling_take.0 != *take {
                    debug!("Rolling on {take} (audio surface {code})");
                }
                self.rolling_take = (take.clone(), gain);
                if let Some(&spacing) = spec.seams.get(code as usize) {
                    self.seam_spacing = spacing;
                }
            }
        }
        let (take, gain) = self.rolling_take.clone();
        if !self.rolling.contains_key(&take) {
            let bands = spec.surfaces.get(&take).filter(|b| !b.is_empty()).unwrap_or(&spec.bands);
            let mut beds = Vec::new();
            for (i, band) in bands.iter().enumerate() {
                let name: &'static str = Box::leak(format!("roll {take} {i}").into_boxed_str());
                let entity = commands
                    .spawn((
                        AudioPlayer::new(server.load::<AudioSource>(format!("private/audio/{}", band.clip))),
                        PlaybackSettings { volume: Volume::Linear(0.), ..PlaybackSettings::LOOP },
                        Name::new(format!("Sound bed: {name}")),
                    ))
                    .id();
                let bed_spec = LoopSpec { clip: Some(band.clip.clone()), volume: spec.volume, pitch: [1., 1.], speed: spec.speed };
                self.beds.insert(name, Bed { entity, spec: bed_spec, level: 0., target: 0., pitch: 1. });
                beds.push((name, band.speed));
            }
            self.rolling.insert(take.clone(), beds);
        }
        let [low, high] = spec.speed;
        let t = ((speed - low) / (high - low).max(1e-3)).clamp(0., 1.);
        let level = amount * gain * (spec.volume[0] + (spec.volume[1] - spec.volume[0]) * t) * (speed / 0.6).clamp(0., 1.);
        for (name, bands) in &self.rolling {
            if bands.is_empty() {
                continue;
            }
            let current = *name == take;
            let upper = bands.iter().position(|(_, s)| *s >= speed).unwrap_or(bands.len() - 1);
            let lower = upper.saturating_sub(1);
            let (s0, s1) = (bands[lower].1, bands[upper].1);
            let mix = if upper == lower { 1. } else { ((speed - s0) / (s1 - s0).max(1e-3)).clamp(0., 1.) };
            for (i, (bed_name, band_speed)) in bands.iter().enumerate() {
                let weight = if !current {
                    0.
                } else if i == upper && i == lower {
                    1.
                } else if i == upper {
                    (mix * std::f32::consts::FRAC_PI_2).sin()
                } else if i == lower {
                    (mix * std::f32::consts::FRAC_PI_2).cos()
                } else {
                    0.
                };
                if let Some(bed) = self.beds.get_mut(bed_name) {
                    bed.target = level * weight;
                    bed.pitch = (speed.max(0.3) / band_speed.max(0.3)).clamp(0.9, 1.1);
                }
            }
        }
    }

    /// A one-shot from a clip list, a random pick.
    fn one(&mut self, commands: &mut Commands, clips: &[Handle<AudioSource>], gain: f32) {
        if clips.is_empty() || self.master <= 0. || gain <= 0. {
            return;
        }
        let pick = self.random();
        let clip = clips[((pick * clips.len() as f32) as usize).min(clips.len() - 1)].clone();
        commands.spawn((
            AudioPlayer::new(clip),
            PlaybackSettings { volume: Volume::Linear(gain * self.master), ..PlaybackSettings::DESPAWN },
            Name::new("Sound"),
        ));
    }

    /// A one-shot from a cue layer: a random variant at the layer's level.
    fn layer(&mut self, commands: &mut Commands, layer: Option<&Layer>, gain: f32) {
        if let Some(layer) = layer {
            let clips = layer.clips.clone();
            self.one(commands, &clips, layer.volume * gain);
        }
    }

    /// A looping bed spawned on first use, by name.
    fn ensure_bed(&mut self, commands: &mut Commands, name: &'static str, clip: Handle<AudioSource>) {
        if self.beds.contains_key(name) {
            return;
        }
        let entity = commands
            .spawn((
                AudioPlayer::new(clip),
                PlaybackSettings { volume: Volume::Linear(0.), ..PlaybackSettings::LOOP },
                Name::new(format!("Sound bed: {name}")),
            ))
            .id();
        let spec = LoopSpec { clip: None, volume: [1., 1.], pitch: [1., 1.], speed: [0., 1.] };
        self.beds.insert(name, Bed { entity, spec, level: 0., target: 0., pitch: 1. });
    }

    fn set_bed(&mut self, name: &str, target: f32, pitch: f32) {
        if let Some(bed) = self.beds.get_mut(name) {
            bed.target = target;
            bed.pitch = pitch;
        }
    }

    /// Weights of `count` layers spread over 0..1, the two around `t`
    /// crossfaded with equal power.
    fn spread(count: usize, t: f32) -> Vec<f32> {
        if count == 0 {
            return Vec::new();
        }
        if count == 1 {
            return vec![1.];
        }
        let pos = t.clamp(0., 1.) * (count - 1) as f32;
        let lower = (pos.floor() as usize).min(count - 1);
        let upper = (lower + 1).min(count - 1);
        let mix = pos - lower as f32;
        (0..count)
            .map(|i| {
                if i == lower && i == upper {
                    1.
                } else if i == lower {
                    (mix * std::f32::consts::FRAC_PI_2).cos()
                } else if i == upper {
                    (mix * std::f32::consts::FRAC_PI_2).sin()
                } else {
                    0.
                }
            })
            .collect()
    }

    /// The speed tier (0-2) of a grind at `speed` m/s against the type's
    /// MaxSpeed (km/h in the game's table).
    fn tier(grind: &Grind, speed: f32) -> usize {
        let frac = (speed * 3.6 / grind.max_speed.max(1.)).clamp(0., 0.999);
        (frac * 3.) as usize
    }

    /// A grind begins on the edge's surface: the enter hit for its speed and
    /// the loop layers, a random variant of each.
    fn grind_start(&mut self, commands: &mut Commands, surface: Option<u32>, slide: bool, speed: f32) {
        let Some(gameplay) = self.gameplay.clone() else { return };
        let Some(grind) = gameplay.grind(surface) else { return };
        let tier = Self::tier(grind, speed);
        let gain = grind.enter_volume[tier];
        if slide && grind.slide_enter.is_some() {
            self.layer(commands, grind.slide_enter.as_ref(), gain);
        } else {
            self.layer(commands, grind.enter.get(tier), gain);
        }
        // Loops: the first three layers roll with speed; a type with more
        // keeps the rest for board slides.
        let layers: Vec<&Clips> = if slide && grind.loops.len() > 3 {
            grind.loops.iter().skip(3).chain(grind.loops.first()).collect()
        } else {
            grind.loops.iter().take(3).collect()
        };
        let variant = (self.random() * 3.) as usize;
        let mut beds = Vec::new();
        for (i, clips) in layers.into_iter().enumerate() {
            let Some(clip) = clips.get(variant % clips.len().max(1)).or(clips.first()) else { continue };
            let name: &'static str = Box::leak(format!("grind {:p} {i} {variant}", grind as *const Grind).into_boxed_str());
            self.ensure_bed(commands, name, clip.clone());
            beds.push(name);
        }
        let index = gameplay.grinds.iter().position(|g| g.as_ref().is_some_and(|g| std::ptr::eq(g, grind))).unwrap_or(usize::MAX);
        self.grind_run = Some((slide, beds, index));
    }

    /// The grind goes on at `speed`: the loop layers crossfaded by speed at
    /// the type's rail volumes.
    fn grind_hold(&mut self, speed: f32) {
        let Some((slide, beds, index)) = self.grind_run.clone() else { return };
        let Some(gameplay) = self.gameplay.clone() else { return };
        let grind = if index == usize::MAX { gameplay.grind_default.as_ref() } else { gameplay.grinds.get(index).and_then(|g| g.as_ref()) };
        let Some(grind) = grind else { return };
        let frac = (speed * 3.6 / grind.max_speed.max(1.)).clamp(0., 1.);
        let weights = Self::spread(beds.len(), frac);
        let level = (speed / 1.5).clamp(0.25, 1.);
        for (i, name) in beds.iter().enumerate() {
            let volume = if slide { grind.rail_volume[i.min(2)] } else { grind.rail_volume[i.min(2)] };
            self.set_bed(name, weights[i] * volume * level, (0.95 + 0.1 * frac).clamp(0.9, 1.1));
        }
    }

    /// The grind ends: its exit hit for the speed, and the loops fade.
    fn grind_stop(&mut self, commands: &mut Commands, speed: f32) {
        let Some((slide, beds, index)) = self.grind_run.take() else { return };
        for name in beds {
            self.set_bed(name, 0., 1.);
        }
        let Some(gameplay) = self.gameplay.clone() else { return };
        let grind = if index == usize::MAX { gameplay.grind_default.as_ref() } else { gameplay.grinds.get(index).and_then(|g| g.as_ref()) };
        let Some(grind) = grind else { return };
        let tier = Self::tier(grind, speed);
        let gain = grind.exit_volume[tier];
        if slide && grind.slide_exit.is_some() {
            self.layer(commands, grind.slide_exit.as_ref(), gain);
        } else {
            self.layer(commands, grind.exit.get(tier), gain);
        }
    }

    /// Play `impact` at `strength`: each layer of that tier (hard landings
    /// add the extra layer), a random variant of each.
    fn impact(&mut self, commands: &mut Commands, impact: Option<&Impact>, strength: Strength, gain: f32) {
        let Some(impact) = impact else { return };
        if self.master <= 0. {
            return;
        }
        let extra = if matches!(strength, Strength::Hard) { &impact.extra[..] } else { &[] };
        let mut picks = Vec::new();
        for layer in impact.tiers[strength as usize].iter().chain(extra) {
            if layer.clips.is_empty() {
                continue;
            }
            picks.push((layer.clips.len(), layer.volume, &layer.clips));
        }
        let rolls: Vec<f32> = picks.iter().map(|_| self.random()).collect();
        for ((count, volume, clips), roll) in picks.into_iter().zip(rolls) {
            let clip = clips[((roll * count as f32) as usize).min(count - 1)].clone();
            commands.spawn((
                AudioPlayer::new(clip),
                PlaybackSettings { volume: Volume::Linear(volume * impact.volume * gain * self.master), ..PlaybackSettings::DESPAWN },
                Name::new("Sound: impact"),
            ));
        }
    }

    fn bed(&mut self, name: &'static str, amount: f32, speed: f32) {
        if let Some(bed) = self.beds.get_mut(name) {
            let [low, high] = bed.spec.speed;
            let t = ((speed - low) / (high - low).max(1e-3)).clamp(0., 1.);
            bed.target = if amount > 0. {
                (bed.spec.volume[0] + (bed.spec.volume[1] - bed.spec.volume[0]) * t) * amount
            } else {
                0.
            };
            bed.pitch = bed.spec.pitch[0] + (bed.spec.pitch[1] - bed.spec.pitch[0]) * t;
        }
    }
}

pub(crate) struct GameAudioPlugin;
impl Plugin for GameAudioPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoundQueue>()
            .add_systems(Startup, load)
            .add_systems(FixedUpdate, follow_physics.after(crate::app::SimulationSet::Physics))
            .add_systems(Update, (play_queued, ambience, mix).chain());
    }
}

fn load(mut commands: Commands, config: Res<crate::config::Config>, server: Res<AssetServer>, player: Res<crate::player_settings::PlayerSettings>) {
    let path = config.asset_root.join("private/audio/sounds.json");
    let table = match skate_data::vfs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Table>(&bytes).ok()) {
        Some(table) if table.version == 1 => table,
        _ => {
            info!("Game audio: no converted sounds ({}); playing silently", path.display());
            return;
        }
    };
    let asset = |clip: &str| server.load::<AudioSource>(format!("private/audio/{clip}"));
    let events = table
        .events
        .iter()
        .map(|(name, spec)| {
            (name.clone(), Event { clips: spec.clips.iter().map(|c| asset(c)).collect(), volume: spec.volume, pitch: spec.pitch })
        })
        .collect();
    let mut beds = HashMap::new();
    let rolling_spec = table.rolling.clone().filter(|spec| !spec.bands.is_empty());
    let impact = |spec: &ImpactSpec| {
        let layers = |list: &[LayerSpec]| {
            list.iter().map(|l| Layer { clips: l.clips.iter().map(|c| asset(c)).collect(), volume: l.volume }).collect()
        };
        Impact { tiers: [layers(&spec.hard), layers(&spec.medium), layers(&spec.soft)], extra: layers(&spec.extra), volume: spec.volume, jump: spec.jump }
    };
    let layers = |list: &[LayerSpec]| -> Vec<Layer> {
        list.iter().map(|l| Layer { clips: l.clips.iter().map(|c| asset(c)).collect(), volume: l.volume }).collect()
    };
    let impacts = table.impacts.as_ref().map(|spec| {
        std::sync::Arc::new(Impacts {
            board: spec.parts.get("board").and_then(|p| p.as_ref()).map(impact),
            truck: spec.parts.get("truck").and_then(|p| p.as_ref()).map(impact),
            surfaces: spec.surfaces.iter().map(|s| s.as_ref().map(impact)).collect(),
            parts: spec.parts.iter().filter_map(|(k, v)| v.as_ref().map(|v| (k.clone(), impact(v)))).collect(),
            hom: spec.hom.iter().map(|(k, v)| (k.clone(), (layers(&v.cues), v.volume))).collect(),
        })
    });
    let clips = |list: &[String]| -> Clips { list.iter().map(|c| asset(c)).collect() };
    let grind = |g: &GrindSpec| Grind {
        enter: layers(&g.enter),
        exit: layers(&g.exit),
        slide_enter: g.slide_enter.as_ref().map(|l| layers(std::slice::from_ref(l)).remove(0)),
        slide_exit: g.slide_exit.as_ref().map(|l| layers(std::slice::from_ref(l)).remove(0)),
        enter_volume: g.enter_volume,
        exit_volume: g.exit_volume,
        rail_volume: g.rail_volume,
        max_speed: g.max_speed,
        loops: g.loops.iter().map(|l| clips(l)).collect(),
    };
    let gameplay = table.gameplay.as_ref().map(|spec| {
        std::sync::Arc::new(Gameplay {
            skids: spec.skids.as_ref().map(|s| s.sets.iter().map(|c| clips(c)).collect()).unwrap_or_default(),
            skid_surface: spec.skids.as_ref().map(|s| s.surface.clone()).unwrap_or_default(),
            wind: spec.wind.as_ref().map(|w| clips(&w.loops)).unwrap_or_default(),
            gusts: spec.wind.as_ref().map(|w| clips(&w.gusts)).unwrap_or_default(),
            wind_speed: spec.wind.as_ref().map(|w| w.speed).unwrap_or([4., 16.]),
            rattles: spec.rattles.iter().map(|c| clips(c)).collect(),
            flips: spec.flips.iter().map(|(k, v)| (k.clone(), clips(v))).collect(),
            scrapes: spec.scrapes.iter().map(|(k, v)| (k.clone(), clips(v))).collect(),
            squeaks: spec.squeaks.iter().map(|c| clips(c)).collect(),
            steps: spec.steps.iter().map(|c| clips(c)).collect(),
            bodyslide: spec.bodyslide.iter().map(|c| clips(c)).collect(),
            cloth: spec.cloth.iter().map(|(k, v)| (k.clone(), clips(v))).collect(),
            grinds: spec.grinds.as_ref().map(|g| g.types.iter().map(|t| t.as_ref().map(grind)).collect()).unwrap_or_default(),
            grind_default: spec.grinds.as_ref().and_then(|g| g.default.as_ref()).map(grind),
            grind_surface: spec.grinds.as_ref().map(|g| g.surface.clone()).unwrap_or_default(),
        })
    });
    let has_gameplay = gameplay.is_some();
    for name in ["roll", "grind", "slide", "air", "wind"] {
        if name == "roll" && rolling_spec.is_some() {
            continue;
        }
        if has_gameplay && name != "air" {
            // Grinds, slides and wind come from the game's own banks.
            continue;
        }
        let Some(spec) = table.loops.get(name).cloned() else { continue };
        let Some(clip) = spec.clip.as_deref() else { continue };
        let entity = commands
            .spawn((
                AudioPlayer::new(asset(clip)),
                PlaybackSettings { volume: Volume::Linear(0.), ..PlaybackSettings::LOOP },
                Name::new(format!("Sound bed: {name}")),
            ))
            .id();
        beds.insert(name, Bed { entity, spec, level: 0., target: 0., pitch: 1. });
    }
    info!("Game audio: {} events, {} beds", table.events.len(), beds.len());
    let mut playlist: Vec<_> = table.maps.iter().map(|(name, clip)| (name.clone(), clip.clone())).collect();
    playlist.sort_by(|a, b| a.0.cmp(&b.0));
    commands.insert_resource(GameAudio {
        events,
        beds,
        random: 0x9e37_79b9_7f4a_7c15,
        master: player.sound_volume as f32 / 100.,
        previous: None,
        air_fall: 0.,
        seam_in: 2.,
        step_in: 0.,
        skid_in: 0.,
        body_in: 0.,
        trick_seq: 0,
        bail_seq: 0,
        rolling: HashMap::new(),
        rolling_spec,
        rolling_take: (String::new(), 1.),
        seam_spacing: [1.8, 3.4],
        impacts,
        gameplay,
        last_surface: None,
        grind_run: None,
        squeak_in: 1.,
        gust_in: 2.,
        whooshed: false,
        flipped: false,
        wheel_hardness: 1.,
        ambience: None,
        playlist,
        music_generation: None,
        music_index: None,
        music_paused: false,
        #[cfg(target_arch = "wasm32")]
        reported_music: None,
    });
}

fn grounded(state: State) -> bool {
    matches!(state, State::PhysicsGround | State::RevertGround | State::GroundAnimation | State::Skitching | State::FollowPath | State::SlideGround)
}
fn airborne(state: State) -> bool {
    matches!(state, State::PhysicsAir | State::KnownAir | State::PhysicsAirSecondary | State::BipedAir)
}
fn grinding(state: State) -> bool {
    matches!(state, State::GrindFiftyFifty | State::GrindTipslide | State::GrindFiveO | State::GrindBackslash | State::GrindDarkslide)
}
/// The board, not the trucks, rides the edge.
fn sliding(state: State) -> bool {
    matches!(state, State::GrindBoardslide | State::GrindTipslide | State::GrindDarkslide)
}
fn on_edge(state: State) -> bool {
    grinding(state) || state == State::GrindBoardslide
}

/// Every fixed tick, after the physics: turn its published state and events
/// into one-shots, and set the beds' targets.
fn follow_physics(
    mut commands: Commands,
    audio: Option<ResMut<GameAudio>>,
    physics: Res<crate::physics::GamePhysics>,
    skater: Res<crate::physics::SkaterRuntime>,
    time: Res<Time<Fixed>>,
    server: Res<AssetServer>,
    modes: Option<Res<crate::game_modes::GameModes>>,
) {
    let Some(mut audio) = audio else { return };
    let Some(output) = physics.physical_output() else { return };
    let dt = time.delta_secs();
    let state = output.state;
    let velocity = output.board_linear_velocity;
    let speed = (velocity.x * velocity.x + velocity.z * velocity.z).sqrt();
    let rider = output.rider_linear_velocity;
    let rider_speed = (rider.x * rider.x + rider.y * rider.y + rider.z * rider.z).sqrt();
    let previous = audio.previous.replace(state);
    if output.audio_surface.is_some() {
        audio.last_surface = output.audio_surface;
    }
    audio.wheel_hardness = physics.wheel_hardness();
    let gameplay = audio.gameplay.clone();
    let hall_of_meat = modes.as_ref().is_some_and(|m| m.active() == Some(crate::game_modes::Kind::HallOfMeat));
    // The surface's class for scrubs and steps: 0 smooth, 1 rough, 2 wood, 3 dirt.
    let surface_class = gameplay
        .as_ref()
        .and_then(|g| audio.last_surface.and_then(|c| g.skid_surface.get(c as usize)).copied())
        .unwrap_or(0) as usize;

    // One-shots from transitions and events.
    if let Some(before) = previous {
        if before != state {
            if airborne(state) && (grounded(before) || grinding(before) || before == State::GrindBoardslide) && velocity.y > 1.2 {
                if let Some(impacts) = audio.impacts.clone() {
                    // The tail strikes the ground: the board's own crack and
                    // the surface's, at the surface's jump level.
                    audio.impact(&mut commands, impacts.board.as_ref(), Strength::Medium, 1.);
                    if let Some(surface) = impacts.surface(audio.last_surface) {
                        audio.impact(&mut commands, Some(surface), Strength::Medium, surface.jump);
                    }
                } else {
                    audio.play(&mut commands, "pop", 1.);
                }
                if let Some(g) = &gameplay {
                    let clips = g.cloth.get("jump").cloned().unwrap_or_default();
                    audio.one(&mut commands, &clips, 0.5);
                }
            }
            if on_edge(state) && !on_edge(before) {
                if gameplay.is_some() {
                    audio.grind_start(&mut commands, output.grind_surface, sliding(state), speed);
                } else {
                    audio.play(&mut commands, "grind_start", 1.);
                }
            } else if on_edge(state) && sliding(state) != sliding(before) {
                // Trucks to board (or back) on the same edge: new loops.
                audio.grind_stop(&mut commands, speed);
                audio.grind_start(&mut commands, output.grind_surface, sliding(state), speed);
            }
            if on_edge(before) && !on_edge(state) {
                if gameplay.is_some() {
                    audio.grind_stop(&mut commands, speed);
                } else {
                    audio.play(&mut commands, "grind_stop", 0.8);
                }
            }
            // A revert scrapes the tail round on the ground.
            if state == State::RevertGround && before != State::RevertGround {
                if let Some(g) = &gameplay {
                    let wood = audio.rolling_take.0.contains("wood");
                    let clips = g.scrapes.get(if wood { "wood" } else { "concrete" }).cloned().unwrap_or_default();
                    audio.one(&mut commands, &clips, (speed / 6.).clamp(0.3, 0.9));
                }
            }
        }
    }
    for event in &output.events {
        match event {
            PhysicsEvent::Landing => {
                if let Some(impacts) = audio.impacts.clone() {
                    // The trucks come down on the surface: both materials'
                    // layers, by how hard.
                    let strength = match audio.air_fall {
                        f if f > 6. => Strength::Hard,
                        f if f > 3. => Strength::Medium,
                        _ => Strength::Soft,
                    };
                    audio.impact(&mut commands, impacts.truck.as_ref(), strength, 1.);
                    let surface = audio.last_surface;
                    audio.impact(&mut commands, impacts.surface(surface), strength, 1.);
                } else {
                    let gain = (0.55 + audio.air_fall / 10.).clamp(0.55, 1.0);
                    audio.play(&mut commands, "land", gain);
                }
                if let Some(g) = &gameplay {
                    if audio.flipped {
                        let clips = g.flips.get("catch").cloned().unwrap_or_default();
                        audio.one(&mut commands, &clips, 0.7);
                    }
                    let clips = g.cloth.get("land").cloned().unwrap_or_default();
                    audio.one(&mut commands, &clips, 0.35);
                }
                audio.flipped = false;
                audio.whooshed = false;
            }
            PhysicsEvent::Wipeout if gameplay.is_none() => audio.play(&mut commands, "bail", 1.),
            PhysicsEvent::Contact { .. } if output.wiping_out && audio.body_in <= 0. => {
                audio.body_in = 0.14;
                match audio.impacts.clone() {
                    Some(impacts) if !impacts.parts.is_empty() => {
                        // The skater hits the ground: a body part's thump and
                        // the surface's, by how fast; a bone on a hard one.
                        let strength = match rider_speed {
                            v if v > 7. => Strength::Hard,
                            v if v > 3.5 => Strength::Medium,
                            _ => Strength::Soft,
                        };
                        let roll = audio.random();
                        let part = if roll < 0.45 { "torso" } else if roll < 0.75 { "leg" } else if roll < 0.95 { "arm" } else { "head" };
                        audio.impact(&mut commands, impacts.parts.get(part), strength, 1.);
                        let surface = audio.last_surface;
                        audio.impact(&mut commands, impacts.surface(surface), strength, 0.8);
                        if matches!(strength, Strength::Hard) && audio.random() < 0.5 {
                            let bone = if audio.random() < 0.7 { "bonecrack" } else { "bonesnap" };
                            audio.impact(&mut commands, impacts.parts.get(bone), Strength::Hard, 1.);
                        }
                        if hall_of_meat {
                            if let Some((cues, volume)) = impacts.hom.get(part) {
                                let pick = (audio.random() * cues.len() as f32) as usize;
                                let layer = cues.get(pick.min(cues.len().saturating_sub(1))).map(|l| Layer { clips: l.clips.clone(), volume: l.volume * volume });
                                audio.layer(&mut commands, layer.as_ref(), 1.);
                            }
                        }
                    }
                    _ => audio.play(&mut commands, "body", 0.8),
                }
            }
            _ => {}
        }
    }
    audio.body_in -= dt;
    if airborne(state) {
        audio.air_fall = audio.air_fall.max(-velocity.y);
    } else {
        audio.air_fall = 0.;
    }

    // A new trick in the air is a flip or a spin of the board.
    let trick_seq = skater.scoring.trick_seq();
    if trick_seq != audio.trick_seq {
        if airborne(state) {
            if let Some(g) = &gameplay {
                let flip = g.flips.get("flip").cloned().unwrap_or_default();
                let wheels = g.flips.get("wheels").cloned().unwrap_or_default();
                let cloth = g.cloth.get("flip").cloned().unwrap_or_default();
                audio.one(&mut commands, &flip, 0.9);
                audio.one(&mut commands, &wheels, 0.5);
                audio.one(&mut commands, &cloth, 0.4);
                audio.flipped = true;
            } else {
                audio.play(&mut commands, "flip", 1.);
            }
        }
        audio.trick_seq = trick_seq;
    }
    audio.bail_seq = skater.scoring.bail_seq;
    // Dropping fast toward a landing: the pre-landing whoosh.
    if airborne(state) && velocity.y < -7. && !audio.whooshed {
        audio.whooshed = true;
        if let Some(g) = &gameplay {
            let clips = g.flips.get("whoosh").cloned().unwrap_or_default();
            audio.one(&mut commands, &clips, 0.6);
        }
    }

    // Textures of riding: pavement seams, powerslide scrubs, footsteps.
    if grounded(state) && state != State::SlideGround && speed > 2. {
        audio.seam_in -= speed * dt;
        if audio.seam_in <= 0. {
            let [near, far] = audio.seam_spacing;
            let gap = near + audio.random() * (far - near);
            audio.seam_in = gap;
            audio.play(&mut commands, "seam", (speed / 10.).clamp(0.4, 1.));
        }
    }
    if state == State::SlideGround && speed > 1. {
        audio.skid_in -= dt;
        if audio.skid_in <= 0. {
            audio.skid_in = 0.06 + audio.random() * 0.04;
            match &gameplay {
                // The surface's scrubs: the softer half at a crawl, the
                // louder half for a real powerslide.
                Some(g) if !g.skids.is_empty() => {
                    let set = g.skids.get(surface_class).or(g.skids.first()).cloned().unwrap_or_default();
                    let half = set.len() / 2;
                    let pool: Vec<Handle<AudioSource>> = if speed < 4.5 && half > 0 { set[..half].to_vec() } else { set[half..].to_vec() };
                    audio.one(&mut commands, &pool, (speed / 8.).clamp(0.4, 1.));
                }
                _ => audio.play(&mut commands, "skid", (speed / 8.).clamp(0.4, 1.)),
            }
        }
    }
    if matches!(state, State::BipedGround | State::OffBoardPushing) && speed > 0.4 {
        audio.step_in -= dt;
        if audio.step_in <= 0. {
            audio.step_in = (0.55 - speed * 0.04).clamp(0.28, 0.55);
            match &gameplay {
                Some(g) if !g.steps.is_empty() => {
                    // Concrete, wood, grit or dirt underfoot.
                    let index = match surface_class { 2 => 1, 1 => 2, 3 => 3, _ => 0 };
                    let set = g.steps.get(index).or(g.steps.first()).cloned().unwrap_or_default();
                    audio.one(&mut commands, &set, 0.8);
                }
                _ => audio.play(&mut commands, "step", 1.),
            }
        }
    }
    // Bushings squeak now and then on a grind.
    if on_edge(state) {
        audio.squeak_in -= dt;
        if audio.squeak_in <= 0. {
            audio.squeak_in = 0.5 + audio.random() * 1.2;
            if let Some(g) = &gameplay {
                if audio.random() < 0.35 && !g.squeaks.is_empty() {
                    let set = ((audio.random() * g.squeaks.len() as f32) as usize).min(g.squeaks.len() - 1);
                    let clips = g.squeaks[set].clone();
                    audio.one(&mut commands, &clips, 0.45);
                }
            }
        }
    } else {
        audio.squeak_in = 0.3;
    }

    // Beds.
    let rolling = if grounded(state) && state != State::SlideGround { 1. } else { 0. };
    audio.roll(&mut commands, &server, rolling, speed, output.audio_surface);
    audio.bed("air", if airborne(state) && state != State::BipedAir { 1. } else { 0. }, speed);
    let full = (speed * speed + velocity.y * velocity.y).sqrt();
    match gameplay {
        Some(g) => {
            if on_edge(state) {
                audio.grind_hold(speed);
            } else if audio.grind_run.is_some() {
                audio.grind_stop(&mut commands, speed);
            }
            // Sense of speed: the wind loops from a breeze to a gale by the
            // skater's full speed, and gusts at the top end.
            let [low, high] = g.wind_speed;
            let frac = ((full - low) / (high - low).max(1e-3)).clamp(0., 1.);
            let weights = GameAudio::spread(g.wind.len(), frac);
            let level = frac.sqrt() * if output.wiping_out { 0.5 } else { 1. };
            for (i, clip) in g.wind.iter().enumerate() {
                let name: &'static str = Box::leak(format!("wind {i}").into_boxed_str());
                audio.ensure_bed(&mut commands, name, clip.clone());
                audio.set_bed(name, weights[i] * level * 0.55, 1.);
            }
            audio.gust_in -= dt;
            if frac > 0.8 && audio.gust_in <= 0. {
                audio.gust_in = 1.5 + audio.random() * 2.5;
                let clips = g.gusts.clone();
                audio.one(&mut commands, &clips, 0.4);
            }
            // The board rattles over rough ground.
            let take = audio.rolling_take.0.clone();
            let rattle = if take.contains("aggregate") { Some(2) } else if take.contains("concrete_rough") { Some(1) } else if take.contains("asphalt_rough") { Some(0) } else { None };
            for (set, clips) in g.rattles.iter().enumerate() {
                let name: &'static str = Box::leak(format!("rattle {set}").into_boxed_str());
                let Some(clip) = clips.first() else { continue };
                audio.ensure_bed(&mut commands, name, clip.clone());
                let on = rolling > 0. && rattle == Some(set.min(g.rattles.len() - 1));
                let level = if on { ((speed - 3.) / 9.).clamp(0., 1.) * 0.5 } else { 0. };
                audio.set_bed(name, level, (0.95 + 0.1 * ((speed - 3.) / 9.).clamp(0., 1.)).clamp(0.9, 1.1));
            }
            // The skater slides along the ground after a bail.
            for (set, clips) in g.bodyslide.iter().enumerate() {
                let name: &'static str = Box::leak(format!("bodyslide {set}").into_boxed_str());
                let Some(clip) = clips.first() else { continue };
                audio.ensure_bed(&mut commands, name, clip.clone());
                let wanted = if surface_class == 2 { 1 } else { 0 };
                let on = output.wiping_out && state == State::WipeoutGround && rider_speed > 1.5 && set == wanted.min(g.bodyslide.len() - 1);
                audio.set_bed(name, if on { ((rider_speed - 1.) / 6.).clamp(0., 1.) * 0.8 } else { 0. }, 1.);
            }
        }
        None => {
            audio.bed("grind", if grinding(state) { 1. } else { 0. }, speed.max(2.));
            audio.bed("slide", if state == State::GrindBoardslide { 1. } else { 0. }, speed.max(2.));
            audio.bed("wind", if output.wiping_out { 0.5 } else { 1. }, full);
        }
    }
}

/// The current map's background bed, replaced when the map changes.
const AMBIENCE_LEVEL: f32 = 0.45;
fn ambience(
    mut commands: Commands,
    audio: Option<ResMut<GameAudio>>,
    map: Res<crate::map_transition::CurrentMap>,
    front: Res<crate::main_menu::MainMenu>,
    server: Res<AssetServer>,
) {
    let Some(mut audio) = audio else { return };
    // The title's own track plays over the front end; generation u64::MAX
    // stands for it.
    let generation = if front.active() { u64::MAX } else { map.generation };
    let stem = if front.active() {
        "title".to_string()
    } else {
        map.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    };
    if audio.music_generation != Some(generation) {
        audio.music_generation = Some(generation);
        audio.music_index = audio.playlist.iter().position(|(name, _)| name.eq_ignore_ascii_case(&stem));
    }

    #[cfg(target_arch = "wasm32")]
    loop {
        match crate::web::take_music_command().as_str() {
            "toggle" => audio.music_paused = !audio.music_paused,
            "next" if !audio.playlist.is_empty() => {
                let len = audio.playlist.len();
                audio.music_index = Some(audio.music_index.unwrap_or(len - 1).wrapping_add(1) % len);
            }
            "previous" if !audio.playlist.is_empty() => {
                let len = audio.playlist.len();
                audio.music_index = Some(audio.music_index.unwrap_or(0).wrapping_add(len - 1) % len);
            }
            "" => break,
            _ => {}
        }
    }

    let track = audio.music_index.filter(|index| *index < audio.playlist.len());
    if !audio
        .ambience
        .as_ref()
        .is_some_and(|playing| playing.generation == generation && playing.track == track)
    {
        if let Some(old) = audio.ambience.take() {
            commands.entity(old.entity).try_despawn();
        }
        let clip = track.and_then(|index| audio.playlist.get(index)).map(|(_, clip)| clip.clone());
        audio.ambience = Some(match clip {
            Some(clip) => {
                let entity = commands
                    .spawn((
                        AudioPlayer::new(server.load::<AudioSource>(format!("private/audio/{clip}"))),
                        PlaybackSettings { volume: Volume::Linear(0.), ..PlaybackSettings::LOOP },
                        Name::new("Sound bed: ambience"),
                    ))
                    .id();
                Ambience { generation, entity, track }
            }
            None => Ambience { generation, entity: Entity::PLACEHOLDER, track: None },
        });
    }

    #[cfg(target_arch = "wasm32")]
    {
        let title = track
            .and_then(|index| audio.playlist.get(index))
            .map_or("", |(name, _)| name.as_str());
        let available = track.is_some();
        let status = (title.to_owned(), audio.music_paused, available);
        if audio.reported_music.as_ref() != Some(&status) {
            crate::web::music_status(title, audio.music_paused, available);
            audio.reported_music = Some(status);
        }
    }
}

fn play_queued(mut commands: Commands, mut queue: ResMut<SoundQueue>, audio: Option<ResMut<GameAudio>>) {
    let requests = std::mem::take(&mut queue.0);
    if let Some(mut audio) = audio {
        for name in requests {
            match fallback(&name) {
                Some(event) if !audio.events.contains_key(name.as_ref()) => audio.play(&mut commands, event, 1.),
                _ => audio.play(&mut commands, &name, 1.),
            }
        }
    }
}

/// Every frame: ease the beds toward their targets, and silence them while
/// the game is paused (the fixed tick, which sets the targets, stops then).
fn mix(
    audio: Option<ResMut<GameAudio>>,
    player: Res<crate::player_settings::PlayerSettings>,
    time: Res<Time<Real>>,
    virtual_time: Res<Time<Virtual>>,
    mut sinks: Query<&mut AudioSink>,
) {
    let Some(mut audio) = audio else { return };
    audio.master = player.sound_volume as f32 / 100.;
    let master = audio.master;
    let paused = virtual_time.is_paused();
    let blend = 1. - (-time.delta_secs() / 0.08).exp();
    if let Some(ambience) = &audio.ambience {
        if let Ok(mut sink) = sinks.get_mut(ambience.entity) {
            let level = sink.volume().to_linear();
            let target = AMBIENCE_LEVEL * master * if paused { 0.5 } else { 1. };
            sink.set_volume(Volume::Linear(level + (target - level) * (1. - (-time.delta_secs() / 1.2).exp())));
            if audio.music_paused {
                sink.pause();
            } else if sink.is_paused() {
                sink.play();
            }
        }
    }
    for bed in audio.beds.values_mut() {
        let target = if paused { 0. } else { bed.target };
        bed.level += (target - bed.level) * blend;
        if let Ok(mut sink) = sinks.get_mut(bed.entity) {
            sink.set_volume(Volume::Linear(bed.level * master));
            sink.set_speed(bed.pitch.max(0.05));
            // A silent bed stops decoding (there are a few dozen rolling loops).
            let silent = target <= 0. && bed.level < 1e-3;
            if silent && !sink.is_paused() {
                sink.pause();
            } else if !silent && sink.is_paused() {
                sink.play();
            }
        }
    }
}
