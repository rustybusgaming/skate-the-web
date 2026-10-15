//! Skate 3's online game modes, as the game ran them: Spot Battle, Hall of
//! Meat and 1-Up, each played at one of its authored challenges (the game's
//! own lists, exported by tools/asset_pipeline/challenges.py from
//! challengebanks/main and challenge_local_data).
//!
//! The rules follow the game's challenge state graphs (Lua 5.1 handlers in
//! data/db/challenge_stategraph): spot_battle and hall_of_meat on
//! turn_based_uniform, and 1up on turn_based_leader.
//!
//! * Spot Battle: players take turns from a random first player, for
//!   NumRounds rounds. The active player spawns at the challenge's
//!   TurnBasedAttemptSpawnPoint, the rest wait at TurnBasedWaitingLocation and
//!   watch. A turn lasts TimeLimit; a bail ends it. A sequence scores only if
//!   it ends inside the spot volumes (or began inside and left them: the
//!   game's ForceEnableScoring). A player's score is their best sequence; a
//!   Session Spot (CumulativeScoring) adds a turn's sequences instead and
//!   ends the turn after NumSequencesToEndTurn of them.
//! * Hall of Meat: the same turns; the turn's slam (scored when the wipeout
//!   is over) is the score, the best slam counts. No bail in time: "ran out
//!   of time".
//! * 1-Up: each round the players still in it take runs of TimeLimit from
//!   the round's start locator (StartLocationRotation), adding up their
//!   sequences. Beat the round's best and you lead; fail once there is a
//!   leader and you take a letter and sit out the round; a bail ends the run.
//!   Three letters (1-U-P) and you are out. The round's leader starts the
//!   next; the last player standing wins.
//! * S.K.A.T.E. (skate graph): the leader sets a flip trick or grab inside
//!   the spot within TimeLimit; one already done (the ABD list) does not
//!   count. If the leader fails (bail, time, out of the spot, repeated) the
//!   lead passes on, with no letter. Every other player then copies the set
//!   trick or takes a letter; once all have tried, the lead passes on.
//!   ABDListItemLimit (5) letters and you are out; last one standing wins.
//! * Domination: everyone at once for TimeLimit. Each of the challenge's
//!   objects (objectives with a ScoringBoundary) belongs to whoever scored
//!   the best sequence in it; your points are the objects you own, the sum
//!   of their scores breaks ties.
//! * Deathrace (race_shared): everyone at once from the heat's start through
//!   its gates, in order (RaceGateSkipable lets you miss some), to the
//!   finish volume within the heat's TimeLimit. Positions by finish time,
//!   then by gates passed; points are competitors minus position.
//!
//! The host (alone, you are the host) runs the round and publishes it as the
//! `mode` record; the active player reports their turn as `mode:me`.
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Kind {
    SpotBattle,
    HallOfMeat,
    OneUp,
    Skate,
    Domination,
    Deathrace,
}

impl Kind {
    pub(crate) const ALL: [Kind; 6] = [Kind::SpotBattle, Kind::HallOfMeat, Kind::OneUp, Kind::Skate, Kind::Domination, Kind::Deathrace];
    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::SpotBattle => "Spot Battle",
            Kind::HallOfMeat => "Hall of Meat",
            Kind::OneUp => "1-Up",
            Kind::Skate => "S.K.A.T.E.",
            Kind::Domination => "Domination",
            Kind::Deathrace => "Deathrace",
        }
    }
    /// Everyone plays at once (no turns).
    pub(crate) fn simultaneous(self) -> bool {
        matches!(self, Kind::Domination | Kind::Deathrace)
    }
    /// The game's own description (ID_MISSION_TEMPLATE_*).
    pub(crate) fn rules(self) -> &'static str {
        match self {
            Kind::SpotBattle => "ID_MISSION_TEMPLATE_ONLINE_SPOTBATTLE_DESCRIPTION",
            Kind::HallOfMeat => "ID_MISSION_TEMPLATE_HALLOFMEAT_DESCRIPTION",
            Kind::OneUp => "ID_MISSION_TEMPLATE_1UP_DESCRIPTION",
            Kind::Skate => "ID_MISSION_TEMPLATE_SKATE_DESCRIPTION",
            Kind::Domination => "ID_MAP_HELPER_OL_COMP_DOMINATION",
            Kind::Deathrace => "ID_MISSION_DEATHRACE_CHALLENGE_DESCRIPTION",
        }
    }
    fn from_mode(mode: &str) -> Option<Self> {
        Some(match mode {
            "spot_battle" => Kind::SpotBattle,
            "hall_of_meat" => Kind::HallOfMeat,
            "one_up" => Kind::OneUp,
            "skate" => Kind::Skate,
            "domination" => Kind::Domination,
            "deathrace" => Kind::Deathrace,
            _ => return None,
        })
    }
}

// ---- the game's challenges --------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
struct Place {
    name: String,
    matrix: Option<[f32; 16]>,
}
impl Place {
    /// The locator as a teleport transform (rows: x, y, z axes, position).
    fn transform(&self) -> Option<[[f32; 4]; 4]> {
        let m = self.matrix?;
        Some([[m[0], m[1], m[2], 0.], [m[4], m[5], m[6], 0.], [m[8], m[9], m[10], 0.], [m[12], m[13], m[14], 0.]])
    }
    fn position(&self) -> Option<Vec3> {
        self.matrix.map(|m| Vec3::new(m[12], m[13], m[14]))
    }
}

/// A trigger volume: an oriented box (rows of `basis` are its axes).
#[derive(Clone, Debug, Default, Deserialize)]
struct Volume {
    #[allow(dead_code)]
    name: String,
    basis: Option<[f32; 9]>,
    center: Option<[f32; 3]>,
    half: Option<[f32; 3]>,
}
impl Volume {
    fn contains(&self, p: Vec3) -> bool {
        let (Some(b), Some(c), Some(h)) = (self.basis, self.center, self.half) else { return false };
        let d = p - Vec3::from_array(c);
        (0..3).all(|i| Vec3::new(b[i * 3], b[i * 3 + 1], b[i * 3 + 2]).dot(d).abs() <= h[i])
    }
    /// A standing point in the volume: its centre, dropped to its floor
    /// when one of its axes is up.
    fn floor(&self) -> Option<Vec3> {
        let (b, c, h) = (self.basis?, Vec3::from_array(self.center?), self.half?);
        for i in 0..3 {
            let axis = Vec3::new(b[i * 3], b[i * 3 + 1], b[i * 3 + 2]);
            if axis.y.abs() > 0.7 {
                return Some(c - axis * axis.y.signum() * (h[i] - 0.3).max(0.));
            }
        }
        Some(c)
    }
}

/// A Deathrace heat: its start, clock, wipeout limit and legs of gates.
#[derive(Clone, Debug, Default, Deserialize)]
struct Heat {
    start: Option<Place>,
    time_limit: Option<f32>,
    #[serde(default)]
    wipeout_limit: Option<f32>,
    #[serde(default)]
    legs: Vec<Leg>,
}
#[derive(Clone, Debug, Default, Deserialize)]
struct Leg {
    #[serde(default)]
    gates: Vec<Volume>,
    #[serde(default)]
    finish: Vec<Volume>,
}

/// A teleport transform standing at `at`, facing `to` (row 2 forward).
fn facing(at: Vec3, to: Vec3) -> [[f32; 4]; 4] {
    let d = Vec3::new(to.x - at.x, 0., to.z - at.z).normalize_or(Vec3::Z);
    [[d.z, 0., -d.x, 0.], [0., 1., 0., 0.], [d.x, 0., d.z, 0.], [at.x, at.y, at.z, 0.]]
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Challenge {
    pub id: String,
    mode: String,
    district: Option<String>,
    title: Option<String>,
    #[serde(default)]
    settings: BTreeMap<String, serde_json::Value>,
    start: Option<Place>,
    spawn: Option<Place>,
    wait: Option<Place>,
    #[serde(default)]
    start_rotation: Vec<Option<Place>>,
    #[serde(default)]
    wait_rotation: Vec<Option<Place>>,
    scoring_boundary: Option<Volume>,
    #[serde(default)]
    spot_volumes: Vec<Volume>,
    boundary: Option<Vec<[f32; 2]>>,
    /// Domination: the objects to own.
    #[serde(default)]
    objects: Vec<Volume>,
    /// Deathrace: the heats (the online races have one).
    #[serde(default)]
    heats: Vec<Heat>,
    #[serde(default)]
    gate_skippable: bool,
}
impl Challenge {
    fn kind(&self) -> Option<Kind> {
        Kind::from_mode(&self.mode)
    }
    fn number(&self, key: &str, default: f32) -> f32 {
        self.settings.get(key).and_then(serde_json::Value::as_f64).map_or(default, |v| v as f32)
    }
    fn flag(&self, key: &str) -> bool {
        self.settings.get(key).and_then(serde_json::Value::as_bool).unwrap_or(false)
    }
    fn time_limit(&self) -> f32 {
        let heat = self.heats.first().and_then(|h| h.time_limit);
        self.number("TimeLimit", heat.unwrap_or(15.)).max(5.)
    }
    /// Deathrace: every gate in order, then the finish volumes.
    fn gates(&self) -> (Vec<&Volume>, Vec<&Volume>) {
        let Some(heat) = self.heats.first() else { return (vec![], vec![]) };
        let gates = heat.legs.iter().flat_map(|l| &l.gates).filter(|v| v.center.is_some()).collect();
        let finish = heat.legs.last().map_or(vec![], |l| l.finish.iter().filter(|v| v.center.is_some()).collect());
        (gates, finish)
    }
    fn wipeout_limit(&self) -> u32 {
        self.heats.first().and_then(|h| h.wipeout_limit).unwrap_or(0.).max(0.) as u32
    }
    fn rounds(&self) -> u32 {
        (self.number("NumRounds", 3.) as u32).clamp(1, 10)
    }
    /// Where the active player starts: the attempt spawn point, or for 1-Up
    /// the round's start locator.
    fn spawn(&self, round: u32) -> Option<&Place> {
        let rotation: Vec<&Place> = self.start_rotation.iter().flatten().filter(|p| p.matrix.is_some()).collect();
        if !rotation.is_empty() {
            return Some(rotation[(round as usize).saturating_sub(1) % rotation.len()]);
        }
        let heat = self.heats.first().and_then(|h| h.start.as_ref());
        [&self.spawn, &self.start].into_iter().flatten().chain(heat).find(|p| p.matrix.is_some())
    }
    fn waiting(&self, round: u32) -> Option<&Place> {
        let rotation: Vec<&Place> = self.wait_rotation.iter().flatten().filter(|p| p.matrix.is_some()).collect();
        if !rotation.is_empty() {
            return Some(rotation[(round as usize).saturating_sub(1) % rotation.len()]);
        }
        self.wait.as_ref().filter(|p| p.matrix.is_some()).or_else(|| self.spawn(round))
    }
    /// The spot: its volumes, else the scoring boundary, else anywhere.
    fn in_spot(&self, p: Vec3) -> bool {
        if !self.spot_volumes.is_empty() {
            return self.spot_volumes.iter().any(|v| v.contains(p));
        }
        self.scoring_boundary.as_ref().is_none_or(|v| v.contains(p))
    }
    /// The challenge area (data/boundary) is intentionally not enforced for
    /// free-flight play: staying on a challenge's authored area should not
    /// forcibly teleport players back when they leave it.
    fn in_area(&self, _p: Vec3) -> bool {
        true
    }
}

#[derive(Deserialize)]
struct ChallengeFile {
    challenges: Vec<Challenge>,
    #[serde(default)]
    hall_of_meat: Option<MeatTables>,
}

/// The few game strings the modes format themselves ({0:s}, {0:points}).
#[derive(Default)]
struct Strings(HashMap<String, String>);
impl Strings {
    fn load(asset_root: &std::path::Path, wanted: &[String]) -> Self {
        let Ok(bytes) = skate_data::vfs::read(asset_root.join("private/frontend/language.json")) else { return Self::default() };
        let Ok(all) = serde_json::from_slice::<HashMap<String, String>>(&bytes) else { return Self::default() };
        Self(wanted.iter().filter_map(|k| all.get(k).map(|v| (k.clone(), v.clone()))).collect())
    }
    fn get(&self, id: &str) -> String {
        self.0.get(id).cloned().unwrap_or_else(|| id.to_owned())
    }
    /// The game's format: `{n:s}` text, `{n:points}`/`{n:d}` grouped numbers.
    fn format(&self, id: &str, args: &[String]) -> String {
        let mut text = self.get(id).replace('\u{ab}', "skate.");
        for (i, arg) in args.iter().enumerate() {
            for spec in ["s", "points", "d", "distance.1"] {
                text = text.replace(&format!("{{{i}:{spec}}}"), arg);
            }
        }
        text.replace('^', " ")
    }
}

const MESSAGES: &[&str] = &[
    "ID_CHALLENGE_CENTRAL_MESSAGE_YOURTURN",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_OTHERSTURN",
    "ID_CHALLENGE_CENTRAL_MESSAGE_SPOTOWNED",
    "ID_CHALLENGE_CENTRALMESSAGE_RANOUTOFTIME",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_NEWSCORESET",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_ROUNDOVER",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_WINS",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_ELIMINATED",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_OTHER_OUTFORROUND",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_ISLEADING",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_SCOREDLOW",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_SCORENOTSET",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_SETYOURSCORE",
    "ID_CHALLENGE_CENTRAL_MESSAGE_MODIFIED_1UP_ONEUPPED",
    "ID_CHALLENGE_CENTRAL_MESSAGE_MODIFIED_1UP_NEWBEST",
    "ID_CHALLENGE_CENTRAL_MESSAGE_1UP_BAILED",
    "ID_CHALLENGE_CENTRAL_MESSAGE_OBJECTIVE_RETURNTOCHAL",
    "ID_CHALLENGE_CENTRAL_MESSAGE_ROUNDOVER",
    "ID_CHALLENGE_COMMON_ROUND_RESULTS",
    "ID_CHALLENGE_COMMON_KILLED_IT",
    "ID_CHALLENGE_CENTRAL_MESSAGE_CHALLENGECOMPLETE",
    "ID_1UP_SET_A_SCORE",
    // S.K.A.T.E.
    "ID_SKATE_IS_LEADER",
    "ID_SKATE_COPY_TRICK",
    "ID_SKATE_NEWLEADER",
    "ID_SKATE_NEWLEADERYOU",
    "ID_SKATE_TRICK_SET",
    "ID_SKATE_TRICK_COPIED",
    "ID_SKATE_TRICK_NOT_COPIED",
    "ID_SKATE_TRICK_ABD",
    "ID_SKATE_PLAYER_BAILED",
    "ID_SKATE_OUT_OF_TIME",
    "ID_SKATE_LEFT_AREA",
    "ID_SKATE_SET_TRICK",
    "ID_SKATE_COPY_THE_TRICK",
    "ID_CHALLENGE_CENTRAL_MESSAGE_NAMEDIDNOTTRICKVALIDAREA",
    "ID_CHALLENGE_CENTRAL_MESSAGE_NAMEGETSALETTER",
    // Domination
    "ID_DOMINATION_CENTRAL_MESSAGE_OWNEDSPOT",
    "ID_DOMINATION_CENTRAL_MESSAGE_LOSTSPOT",
    "ID_DOMINATION_CENTRAL_MESSAGE_SCOREINCREASED",
    "ID_DOMINATION_CENTRAL_MESSAGE_DOMINATING",
    "ID_CONTEST_MODCENTRALMESSAGE_NEWLEAD",
    "ID_CONTEST_CENTRALMESSAGE_NEWLEADYOU",
    // Deathrace
    "ID_RACE_GATES_MISSED",
    "ID_RACE_FINISHED",
    "ID_RACE_CENTRAL_MESSAGE_WAITING_FOR_FINISH",
    "ID_CHALLENGE_RACE_OFFPATH",
    "ID_RACE_MISSED_TOO_MANY_GATES",
    "ID_RACE_DNF",
    "ID_LEADERBOARD_LOST_RACE",
];

fn points(v: f32) -> String {
    crate::apt_screen::group(v.round() as i64).trim_start_matches('#').to_string()
}

// ---- the round ----------------------------------------------------------------

const ROUND_KEY: &str = "mode";
/// How long a mode waits for the session hosted for it before starting alone.
const HOSTING_WAIT: f32 = 12.;
const MINE_KEY: &str = "mode:me";
/// Countdown after the CountdownDelay: 3, 2, 1, GO.
const COUNTDOWN: f32 = 3.;
const END_TURN: f32 = 3.;
const RESULTS: f32 = 8.;
/// Spell 1-UP.
const MAX_LETTERS: u8 = 3;
/// S.K.A.T.E.'s letters.
const SKATE: &str = "SKATE";
/// How long a letter plays before the turn's message (ShowLettersTimerID).
const LETTERS_TIME: f32 = 3.;
/// Flip tricks and grabs (scorable TrickType 2 flip, 3 fingerflip, 4 grab)
/// can be set; ollies, grinds and the rest cannot.
const SETTABLE: [u32; 3] = [2, 3, 4];
/// A host never waits forever for an active player who went quiet.
const TURN_GRACE: f32 = 12.;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    Countdown,
    Running,
    EndTurn,
    Finished,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Player {
    id: u64,
    score: f32,
    letters: u8,
    eliminated: bool,
    /// Domination: the sum of owned objects' scores (the tiebreak).
    /// Deathrace: gates passed.
    #[serde(default)]
    extra: f32,
    /// Deathrace: the finish time in seconds, or did not finish.
    #[serde(default)]
    finished: Option<f32>,
    #[serde(default)]
    dnf: bool,
}

/// A trick to set or copy (S.K.A.T.E.): its name and the stick gesture
/// hudabd draws for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SetTrick {
    name: String,
    gesture: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Fail {
    Bailed,
    OutOfTime,
    OutOfSpot,
    LeftArea,
    Repeated,
}

/// The round as the host runs it (the `mode` record).
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Round {
    id: u32,
    kind: Kind,
    challenge: String,
    state: State,
    /// Time left in the state when published (ms).
    left_ms: u64,
    round: u32,
    /// Turns so far, the current included: each is one active player's go.
    turn: u32,
    active: u64,
    players: Vec<Player>,
    /// Turn order (player ids).
    order: Vec<u64>,
    cursor: usize,
    first: usize,
    /// 1-Up: who is still in this round, its best run and who set it.
    in_round: Vec<u64>,
    best: f32,
    leader: Option<u64>,
    /// The last turn's outcome, for every client's message.
    outcome: Option<Outcome>,
    /// S.K.A.T.E.: the trick to copy, those already done, and who still
    /// has to copy it.
    #[serde(default)]
    set_trick: Option<SetTrick>,
    #[serde(default)]
    abd: Vec<SetTrick>,
    #[serde(default)]
    queue: Vec<u64>,
    /// The last copy attempt, for hudabd's success/failed beat.
    #[serde(default)]
    attempt: Option<SetTrick>,
    /// Domination: each object's owner and the score that owns it.
    #[serde(default)]
    objects: Vec<Option<(u64, f32)>>,
    /// Counts outcomes, so two alike in a row are both shown.
    #[serde(default)]
    events: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Outcome {
    Scored { id: u64, score: f32, owned: bool },
    TimeUp { id: u64 },
    NewScore { id: u64, score: f32, one_upped: bool },
    Letter { id: u64, letters: u8, eliminated: bool },
    NotSet { id: u64 },
    SkateSet { id: u64, trick: String },
    SkateFail { id: u64, fail: Fail },
    SkateCopied { id: u64, everyone: bool },
    SkateMissed { id: u64, letters: u8, eliminated: bool, fail: Option<Fail> },
    Owned { n: u32, id: u64, object: usize, from: Option<u64> },
    Finished { n: u32, id: u64, position: u32, dnf: bool },
}

/// The active player's report on their turn (the `mode:me` record).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Turn {
    round: u32,
    turn: u32,
    score: f32,
    done: bool,
    timed_out: bool,
    /// S.K.A.T.E.: the trick landed, or why the attempt failed.
    #[serde(default)]
    trick: Option<SetTrick>,
    #[serde(default)]
    fail: Option<Fail>,
    /// Domination: the best sequence in each object.
    #[serde(default)]
    claims: Vec<f32>,
    /// Deathrace: the next gate, gates missed, wipeouts and the finish.
    #[serde(default)]
    gate: u32,
    #[serde(default)]
    missed: u32,
    #[serde(default)]
    wipeouts: u32,
    #[serde(default)]
    finished: Option<f32>,
    #[serde(default)]
    dnf: bool,
}

/// Hall of Meat's slam scoring, from the game's scoring_wipeouts (exported
/// beside the challenges): bonus curves by name, and per bone its damage
/// levels (points from an impact up) and the share of a blow it passes on.
#[derive(Clone, Debug, Default, Deserialize)]
struct MeatTables {
    #[serde(default)]
    curves: BTreeMap<String, Curve>,
    #[serde(default)]
    bones: Vec<BoneTable>,
}
/// Sk8::PointNegGraphData8: eight points, linear between them, flat past
/// either end (a repeated x is a step).
#[derive(Clone, Debug, Default, Deserialize)]
struct Curve {
    x: Vec<f32>,
    y: Vec<f32>,
}
impl Curve {
    fn at(&self, v: f32) -> f32 {
        let n = self.x.len().min(self.y.len());
        if n == 0 {
            return 0.;
        }
        if v <= self.x[0] {
            return self.y[0];
        }
        for i in 1..n {
            if v <= self.x[i] {
                let span = self.x[i] - self.x[i - 1];
                if span <= 0. {
                    return self.y[i];
                }
                return self.y[i - 1] + (self.y[i] - self.y[i - 1]) * (v - self.x[i - 1]) / span;
            }
        }
        self.y[n - 1]
    }
}
/// Sk8::Score::HoM::BoneSetAttribData's entry for one bone.
#[derive(Clone, Debug, Default, Deserialize)]
struct BoneTable {
    #[serde(default)]
    spread: Vec<(usize, f32)>,
    /// (points, impact) for Scraped .. Broken; points 0 is a level this bone lacks.
    #[serde(default)]
    levels: Vec<(f32, f32)>,
}
impl MeatTables {
    fn curve(&self, name: &str, v: f32) -> f32 {
        self.curves.get(name).map_or(0., |c| c.at(v))
    }
    /// A bone's points: the highest level its worst impact reached.
    fn bone_points(&self, bone: usize, impact: f32) -> f32 {
        self.bones.get(bone).map_or(0., |b| b.levels.iter().filter(|(p, t)| *p > 0. && impact >= *t).map(|(p, _)| *p).fold(0., f32::max))
    }
    /// A blow to a bone, passed on in part to its neighbours (one step).
    fn hit(&self, impacts: &mut [f32], bone: usize, impact: f32) {
        if let Some(worst) = impacts.get_mut(bone) {
            *worst = worst.max(impact);
        }
        for &(next, share) in self.bones.get(bone).map_or(&[][..], |b| &b.spread[..]) {
            if let Some(worst) = impacts.get_mut(next) {
                *worst = worst.max(impact * share);
            }
        }
    }
}

/// The ragdoll's parts (physics-skeletons order) are the Hall of Meat bones
/// one up: part 1 (NECK1) is bone 0 .. part 23 (HIPS) is bone 22. Bones 23
/// (skull) and 24 (groin) are the game's special hit zones.
const RAGDOLL_PARTS: usize = 24;
const HIPS: usize = 23;

/// A wipeout as it happens: the figures Hall of Meat scores.
#[derive(Default)]
struct Bail {
    speed: f32,
    top: f32,
    bottom: f32,
    air: f32,
    longest_air: f32,
    down: f32,
    rotation: f32,
    velocities: Vec<Vec3>,
    impacts: Vec<f32>,
}
impl Bail {
    /// The slam: body damage (each bone's level points) plus the bonus
    /// curves for speed, drop, drop time, bail time and rotation.
    fn slam(&self, tables: &MeatTables) -> crate::mode_hud::Slam {
        let drop = (self.top - self.bottom).max(0.);
        let body: f32 = self.impacts.iter().enumerate().map(|(bone, &impact)| tables.bone_points(bone, impact)).sum();
        let metrics = [self.speed * 3.6, drop, self.longest_air, self.down, self.rotation];
        let scores = [
            tables.curve("BailSpeedScore", self.speed),
            tables.curve("DropHeightScore", drop),
            tables.curve("DropTimeScore", self.longest_air),
            tables.curve("BailDurationScore", self.down),
            tables.curve("BailRotationScore", self.rotation),
        ];
        let score = body + scores.iter().sum::<f32>();
        crate::mode_hud::Slam { score: score.round() as f64, metrics: metrics.map(|m| m as f64), scores: scores.map(|s| s.round() as f64) }
    }
}

/// Waiting players watch the active one: no gameplay, the camera on them.
static WAITING: AtomicBool = AtomicBool::new(false);
static CAMERA: std::sync::Mutex<Option<Transform>> = std::sync::Mutex::new(None);
pub(crate) fn waiting() -> bool {
    WAITING.load(Ordering::Relaxed)
}
pub(crate) fn camera_override() -> Option<Transform> {
    *CAMERA.lock().unwrap_or_else(|e| e.into_inner())
}

/// What to start: a mode at one of its challenges (None: a random one).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Start {
    pub kind: Kind,
    pub challenge: Option<usize>,
}

#[derive(Resource, Default)]
pub(crate) struct GameModes {
    catalog: Vec<Challenge>,
    loaded: bool,
    strings: Strings,
    map: Option<String>,
    round: Option<Round>,
    left: f32,
    /// This player's own turn.
    mine: Turn,
    turn_left: f32,
    touched_spot: bool,
    /// Domination: the objects the sequence in progress passed through.
    touched_objects: Vec<bool>,
    was_wiping: bool,
    /// The leader shown last, for lead-change messages.
    seen_leader: Option<u64>,
    sequences: u32,
    landing_seq: u32,
    outside_for: f32,
    bail: Option<Bail>,
    meat: MeatTables,
    /// The ragdoll parts' velocities last tick, and the fall in progress
    /// (highest point, time in the air), so the blow and the drop that
    /// start a wipeout count toward it.
    part_velocities: Vec<Vec3>,
    fall_top: Option<f32>,
    fall_time: f32,
    fall_speed: f32,
    grounded: f32,
    seen_turn: u32,
    seen_outcome: Option<Outcome>,
    publish_in: f32,
    request: Option<Option<Start>>,
    /// A mode waiting for its hosted session, and how long it still waits.
    hosted: Option<(Start, f32)>,
    local: u64,
    names: BTreeMap<u64, String>,
    /// Central message: text, seconds left, colour (0 white, 1 red, 2 celebratory).
    message: Option<(String, f32, u32)>,
    stinger: crate::mode_hud::Stinger,
    /// The last letter taken, for hudthrowmes (state, count).
    letter: Option<(u32, u32)>,
    regular: bool,
    /// A message held back while a letter plays (the game's 3 s letters
    /// timer): text, colour, seconds to wait.
    queued: Option<(String, u32, f32)>,
    slam: Option<crate::mode_hud::Slam>,
    slam_left: f32,
}

impl GameModes {
    pub(crate) fn request(&mut self, start: Option<Start>) {
        self.request = Some(start);
    }
    /// Start a mode in the session being hosted for it, once it is up (or
    /// alone, if hosting does not come up).
    pub(crate) fn request_hosted(&mut self, start: Start) {
        self.hosted = Some((start, HOSTING_WAIT));
    }
    pub(crate) fn active(&self) -> Option<Kind> {
        self.round.as_ref().map(|r| r.kind)
    }
    pub(crate) fn round_kind(&self) -> Option<Kind> {
        self.active()
    }
    /// A central message from elsewhere in the game (multiplayer notices).
    pub(crate) fn announce(&mut self, text: impl Into<String>, seconds: f32) {
        self.say(text, seconds);
    }
    fn say(&mut self, text: impl Into<String>, seconds: f32) {
        self.message = Some((text.into(), seconds, 0));
    }
    /// The game's message colours: red stays red; its green (this player's
    /// own news) shows as the standard message, as the celebratory one only
    /// plays with its Killed It / Owned It stinger (`celebrate`).
    fn say_coloured(&mut self, text: impl Into<String>, seconds: f32, colour: u32) {
        self.message = Some((text.into(), seconds, if colour == 1 { 1 } else { 0 }));
    }
    /// hudthrowmes plays a letter: S.K.A.T.E. 1-5, 1-U-P 6-8.
    fn show_letter(&mut self, state: u32) {
        let count = self.letter.map_or(1, |(_, n)| n.wrapping_add(1));
        self.letter = Some((state, count));
    }
    fn celebrate(&mut self, text: impl Into<String>, seconds: f32, owned: bool) {
        self.message = Some((text.into(), seconds, 2));
        self.stinger = crate::mode_hud::Stinger { seq: self.stinger.seq.wrapping_add(1).max(1), owned };
    }
    /// This map's challenges of a mode, by title, for the menu.
    pub(crate) fn spots(&self, kind: Kind) -> Vec<String> {
        self.on_map(kind).iter().map(|c| self.title(c)).collect()
    }
    fn on_map(&self, kind: Kind) -> Vec<&Challenge> {
        let Some(map) = &self.map else { return vec![] };
        self.catalog
            .iter()
            .filter(|c| c.kind() == Some(kind) && c.district.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(map)))
            .filter(|c| c.spawn(1).is_some())
            .collect()
    }
    fn title(&self, c: &Challenge) -> String {
        // A few challenges ship without a title: the mode and its number.
        let untitled = || {
            let number = c.id.rsplit('_').next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(1);
            format!("{} {number}", c.kind().map_or("Challenge", Kind::label))
        };
        c.title.as_deref().map_or_else(untitled, |t| self.strings.get(t))
    }
    fn challenge(&self) -> Option<&Challenge> {
        let round = self.round.as_ref()?;
        self.catalog.iter().find(|c| c.id == round.challenge)
    }
    fn name(&self, id: u64) -> String {
        if id == self.local {
            return "You".into();
        }
        self.names.get(&id).cloned().unwrap_or_else(|| "Player".into())
    }
    fn load(&mut self, asset_root: &std::path::Path) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        match skate_data::vfs::read(asset_root.join("private/frontend/challenges.json")).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<ChallengeFile>(&b).map_err(|e| e.to_string())) {
            Ok(file) => {
                self.catalog = file.challenges;
                self.meat = file.hall_of_meat.unwrap_or_default();
            }
            Err(error) => info!("Game modes: no challenge data ({error}); run tools/asset_pipeline/challenges.py"),
        }
        let mut wanted: Vec<String> = MESSAGES.iter().map(|s| s.to_string()).collect();
        wanted.extend(self.catalog.iter().filter_map(|c| c.title.clone()));
        self.strings = Strings::load(asset_root, &wanted);
        info!("GAME_MODES_READY challenges={}", self.catalog.len());
    }
}

// ---- the host -----------------------------------------------------------------

impl Round {
    fn player(&mut self, id: u64) -> Option<&mut Player> {
        self.players.iter_mut().find(|p| p.id == id)
    }
}

/// Start a round: the order begins at a random player (the game's
/// RandomStartIndex); 1-Up starts with everyone in the first round.
fn begin(modes: &mut GameModes, start: Start, players: Vec<u64>, seed: u64) -> Result<(), String> {
    let spots = modes.on_map(start.kind);
    if spots.is_empty() {
        return Err(format!("No {} spots on this map.", start.kind.label()));
    }
    let challenge = match start.challenge {
        Some(i) => spots.get(i).copied().ok_or("No such spot")?,
        None => spots[(seed as usize) % spots.len()],
    }
    .clone();
    let first = (seed as usize / 7) % players.len().max(1);
    let id = modes.round.as_ref().map_or(1, |r| r.id.wrapping_add(1).max(1));
    let round = Round {
        id,
        kind: start.kind,
        challenge: challenge.id.clone(),
        state: State::Countdown,
        left_ms: 0,
        round: 1,
        turn: 1,
        active: if start.kind.simultaneous() { u64::MAX } else { players[first] },
        players: players.iter().map(|&id| Player { id, score: 0., letters: 0, eliminated: false, extra: 0., finished: None, dnf: false }).collect(),
        order: players.clone(),
        cursor: first,
        first,
        in_round: players.clone(),
        best: 0.,
        leader: (start.kind == Kind::Skate).then_some(players[first]),
        outcome: None,
        set_trick: None,
        abd: vec![],
        queue: vec![],
        attempt: None,
        objects: vec![None; challenge.objects.len()],
        events: 0,
    };
    modes.left = challenge.number("CountdownDelay", 1.5) + COUNTDOWN;
    info!("GAME_MODE_START kind={:?} challenge={} players={}", start.kind, challenge.id, players.len());
    modes.round = Some(round);
    modes.seen_turn = 0;
    modes.seen_outcome = None;
    Ok(())
}

/// The host takes the active player's report and moves the round on.
fn end_turn(modes: &mut GameModes, report: &Turn) {
    let max_letters = challenge_letters(modes.challenge());
    let Some(round) = modes.round.as_mut() else { return };
    let id = round.active;
    match round.kind {
        Kind::SpotBattle | Kind::HallOfMeat => {
            let best = round.players.iter().map(|p| p.score).fold(0., f32::max);
            let player = round.player(id).unwrap();
            let improved = report.score > player.score;
            if improved {
                player.score = report.score;
            }
            round.outcome = Some(if report.timed_out && report.score <= 0. {
                Outcome::TimeUp { id }
            } else {
                Outcome::Scored { id, score: report.score, owned: improved && report.score > best }
            });
        }
        Kind::OneUp => {
            let run = report.score;
            if run > round.best && run > 0. {
                let one_upped = round.leader.is_some();
                round.best = run;
                round.leader = Some(id);
                round.outcome = Some(Outcome::NewScore { id, score: run, one_upped });
                let player = round.player(id).unwrap();
                player.score = player.score.max(run);
            } else if round.leader.is_some() {
                let player = round.player(id).unwrap();
                player.letters = (player.letters + 1).min(MAX_LETTERS);
                player.eliminated = player.letters >= MAX_LETTERS;
                let (letters, eliminated) = (player.letters, player.eliminated);
                if round.players.len() > 1 {
                    round.in_round.retain(|&p| p != id);
                }
                round.outcome = Some(Outcome::Letter { id, letters, eliminated });
            } else {
                round.outcome = Some(Outcome::NotSet { id });
            }
        }
        Kind::Skate => {
            let order = round.order.clone();
            let standing: Vec<u64> = round.players.iter().filter(|p| !p.eliminated).map(|p| p.id).collect();
            if round.set_trick.is_none() {
                // The leader sets: a new trick, landed in the spot, in time.
                let repeated = report.trick.as_ref().is_some_and(|t| round.abd.iter().any(|a| a.name == t.name));
                match (&report.trick, report.fail) {
                    (Some(trick), None) if !repeated => {
                        round.set_trick = Some(trick.clone());
                        round.abd.push(trick.clone());
                        round.outcome = Some(Outcome::SkateSet { id, trick: trick.name.clone() });
                        // Everyone else copies, in order after the leader;
                        // alone, you copy your own trick.
                        let at = order.iter().position(|&p| p == id).unwrap_or(0);
                        round.queue = (1..=order.len())
                            .map(|i| order[(at + i) % order.len()])
                            .filter(|p| *p != id && standing.contains(p))
                            .collect();
                        if order.len() == 1 {
                            round.queue = vec![id];
                        }
                    }
                    (_, fail) => {
                        let fail = if repeated { Fail::Repeated } else { fail.unwrap_or(Fail::OutOfTime) };
                        round.outcome = Some(Outcome::SkateFail { id, fail });
                        round.leader = Some(next_after(&order, &standing, id));
                    }
                }
            } else {
                // A follower copies the set trick or takes a letter.
                round.attempt = report.trick.clone();
                let copied = report.fail.is_none() && report.trick.is_some() && report.trick.as_ref().map(|t| &t.name) == round.set_trick.as_ref().map(|t| &t.name);
                if copied {
                    round.outcome = Some(Outcome::SkateCopied { id, everyone: round.queue.is_empty() });
                } else {
                    let player = round.player(id).unwrap();
                    player.letters = (player.letters + 1).min(max_letters);
                    player.eliminated = player.letters >= max_letters;
                    let (letters, eliminated) = (player.letters, player.eliminated);
                    round.outcome = Some(Outcome::SkateMissed { id, letters, eliminated, fail: report.fail });
                }
            }
        }
        Kind::Domination | Kind::Deathrace => {}
    }
    round.state = State::EndTurn;
    // A letter plays first, then the turn's message.
    let letter = matches!(round.outcome, Some(Outcome::Letter { .. } | Outcome::SkateMissed { .. }));
    modes.left = END_TURN + if letter { LETTERS_TIME } else { 0. };
}

/// S.K.A.T.E.'s letters to lose (ABDListItemLimit, the length of S.K.A.T.E.).
fn challenge_letters(challenge: Option<&Challenge>) -> u8 {
    challenge.map_or(5., |c| c.number("ABDListItemLimit", 5.)).clamp(1., SKATE.len() as f32) as u8
}

/// The next player still standing after `id` in turn order (or `id`).
fn next_after(order: &[u64], standing: &[u64], id: u64) -> u64 {
    let at = order.iter().position(|&p| p == id).unwrap_or(0);
    (1..=order.len()).map(|i| order[(at + i) % order.len()]).find(|p| standing.contains(p)).unwrap_or(id)
}

/// Domination and Deathrace: the host folds every player's report into
/// the standings as they play.
fn tally(modes: &mut GameModes, reports: &[(u64, Turn)]) {
    let Some(round) = modes.round.as_mut() else { return };
    let reports: Vec<&(u64, Turn)> = reports.iter().filter(|(_, t)| t.round == round.id).collect();
    match round.kind {
        Kind::Domination => {
            for i in 0..round.objects.len() {
                // The best sequence owns it; a tie stays with the owner.
                let owner = round.objects[i];
                let mut best = owner;
                for (id, turn) in &reports {
                    let claim = turn.claims.get(i).copied().unwrap_or(0.);
                    if claim > 0. && best.is_none_or(|(_, s)| claim > s) {
                        best = Some((*id, claim));
                    }
                }
                if let Some((id, _)) = best.filter(|b| owner.is_none_or(|o| o.0 != b.0)) {
                    round.events += 1;
                    round.outcome = Some(Outcome::Owned { n: round.events, id, object: i, from: owner.map(|o| o.0) });
                }
                round.objects[i] = best;
            }
            for p in &mut round.players {
                let owned: Vec<f32> = round.objects.iter().flatten().filter(|(o, _)| *o == p.id).map(|(_, s)| *s).collect();
                p.score = owned.len() as f32;
                p.extra = owned.iter().sum();
            }
        }
        Kind::Deathrace => {
            for (id, turn) in &reports {
                let Some(p) = round.players.iter_mut().find(|p| p.id == *id) else { continue };
                p.extra = turn.gate as f32;
                if p.finished.is_none() && !p.dnf && (turn.finished.is_some() || turn.dnf) {
                    (p.finished, p.dnf) = (turn.finished, turn.dnf);
                    let position = round.players.iter().filter(|q| q.finished.is_some()).count() as u32;
                    round.events += 1;
                    round.outcome = Some(Outcome::Finished { n: round.events, id: *id, position, dnf: turn.dnf });
                }
            }
            // Places: finishers by time, then by gates passed; points are
            // the competitors behind you (NumCompetitors - position).
            let n = round.players.len();
            let mut order: Vec<usize> = (0..n).collect();
            order.sort_by(|&a, &b| race_order(&round.players[a], &round.players[b]));
            for (place, i) in order.into_iter().enumerate() {
                round.players[i].score = (n - place - 1) as f32;
            }
        }
        _ => {}
    }
}

fn race_order(a: &Player, b: &Player) -> std::cmp::Ordering {
    let key = |p: &Player| (p.dnf, p.finished.is_none(), p.finished.unwrap_or(0.), -p.extra);
    let (ka, kb) = (key(a), key(b));
    ka.0.cmp(&kb.0).then(ka.1.cmp(&kb.1)).then(ka.2.total_cmp(&kb.2)).then(ka.3.total_cmp(&kb.3))
}

/// hudabd's stick gesture clip (g_<gesture>) for a flip trick's scorable;
/// grabs have none, so the panel shows the trick's name alone.
fn gesture(identifier: &str) -> String {
    let (nollie, core) = match identifier.strip_prefix("n_") {
        Some(core) => (true, core),
        None => (false, identifier),
    };
    let name = match core.trim_end_matches(|c: char| c.is_ascii_digit()) {
        "kickflip" => "kickflip",
        "heelflip" => "heelflip",
        "hardflip" => "hardflip",
        "inwardheelflip" => "inward_heelflip",
        "360flip" => "360_flip",
        "360hardflip" => "360_hardflip",
        "360heelflip" => "360_heelflip",
        "360inwardheelflip" if nollie => "360_inward_heel",
        "360inwardheelflip" => "360_inward_heelflip",
        "popshuvit" => "popshuvit",
        "fspopshuvit" => "fs_popshuvit",
        "360popshuvit" => "360popshuvit",
        "fs360popshuvit" => "fs_360popshuvit",
        "latebsshuv" => "popshuvit_late",
        "latefsshuv" => "fs_popshuvit_late",
        "varialkickflip" if nollie => "varial_flip",
        "varialkickflip" => "varial_kickflip",
        "varialheelflip" => "varial_heelflip",
        _ => return "trick".into(),
    };
    format!("{}_{name}", if nollie { "nollie" } else { "ollie" })
}

/// After a turn's result: the next player, round or the end.
fn next_turn(modes: &mut GameModes, connected: &[u64]) {
    let Some(challenge) = modes.challenge().cloned() else { return };
    let Some(round) = modes.round.as_mut() else { return };
    let finished = match round.kind {
        Kind::SpotBattle | Kind::HallOfMeat => {
            // Next connected player; wrapping to the first player is a new round.
            let mut finished = false;
            for _ in 0..round.order.len() {
                round.cursor = (round.cursor + 1) % round.order.len();
                if round.cursor == round.first {
                    round.round += 1;
                    if round.round > challenge.rounds() {
                        finished = true;
                        break;
                    }
                }
                if connected.contains(&round.order[round.cursor]) {
                    break;
                }
            }
            finished
        }
        Kind::OneUp => {
            let standing: Vec<u64> = round.players.iter().filter(|p| !p.eliminated && connected.contains(&p.id)).map(|p| p.id).collect();
            round.in_round.retain(|id| connected.contains(id));
            if round.players.len() == 1 {
                // Alone: every run must beat your best; three misses end it.
                standing.is_empty()
            } else if standing.len() <= 1 {
                true
            } else if round.in_round.len() < 2 {
                // The round is over: its leader starts the next one.
                let starter = round.leader.unwrap_or(round.active);
                round.round += 1;
                round.in_round = standing.clone();
                round.best = 0.;
                round.leader = None;
                round.cursor = round.in_round.iter().position(|&p| p == starter).unwrap_or(0);
                false
            } else {
                // The next player still in the round, after this one.
                round.cursor = match round.in_round.iter().position(|&p| p == round.active) {
                    Some(i) => (i + 1) % round.in_round.len(),
                    None => round.cursor % round.in_round.len(),
                };
                false
            }
        }
        Kind::Skate => {
            let standing: Vec<u64> = round.players.iter().filter(|p| !p.eliminated && connected.contains(&p.id)).map(|p| p.id).collect();
            round.queue.retain(|p| standing.contains(p));
            if standing.is_empty() || round.players.len() > 1 && standing.len() <= 1 {
                true
            } else {
                if round.set_trick.is_some() && round.queue.is_empty() {
                    // Everyone has tried: the lead passes on and a new trick is set.
                    let leader = round.leader.unwrap_or(round.active);
                    round.set_trick = None;
                    round.leader = Some(next_after(&round.order, &standing, leader));
                    round.round += 1;
                }
                if round.leader.is_none_or(|l| !standing.contains(&l)) {
                    round.leader = Some(next_after(&round.order, &standing, round.leader.unwrap_or(round.active)));
                }
                false
            }
        }
        Kind::Domination | Kind::Deathrace => true,
    };
    if finished {
        round.state = State::Finished;
        modes.left = RESULTS;
        return;
    }
    round.active = match round.kind {
        Kind::OneUp => round.in_round.get(round.cursor).copied().unwrap_or(round.active),
        Kind::Skate => match round.set_trick {
            Some(_) => round.queue.remove(0),
            None => round.leader.unwrap_or(round.active),
        },
        _ => round.order[round.cursor],
    };
    round.turn += 1;
    round.outcome = None;
    round.attempt = None;
    round.state = State::Countdown;
    modes.left = challenge.number("CountdownDelay", 1.5) + COUNTDOWN;
}

// ---- every client --------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn play(
    mut modes: ResMut<GameModes>,
    mut net: ResMut<crate::multiplayer::Multiplayer>,
    physics: Res<crate::physics::GamePhysics>,
    mut skater: ResMut<crate::physics::SkaterRuntime>,
    config: Res<crate::config::Config>,
    mut sounds: ResMut<crate::game_audio::SoundQueue>,
    time: Res<Time<Fixed>>,
) {
    let dt = time.delta_secs();
    modes.load(&config.asset_root);
    modes.regular = skater.animation.packet.regular_stance;
    modes.map = config.map_path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned());
    let session = net.session_clock();
    let (local, host, now) = session.unwrap_or((0, true, 0));
    modes.local = local;
    let players: Vec<u64> = if session.is_some() { net.player_ids() } else { vec![local] };
    modes.names = players.iter().map(|&id| (id, net.display_name(id))).collect();
    let Some(output) = physics.physical_output() else { return };
    let position = Vec3::new(output.board_position.x, output.board_position.y, output.board_position.z);

    if let Some((_, left, _)) = &mut modes.message {
        *left -= dt;
        if *left <= 0. {
            modes.message = None;
        }
    }
    if let Some((text, colour, wait)) = &mut modes.queued {
        *wait -= dt;
        if *wait <= 0. {
            let (text, colour) = (std::mem::take(text), *colour);
            modes.queued = None;
            modes.say_coloured(text, 2.5, colour);
        }
    }
    if modes.bail.is_none() && modes.slam.is_some() {
        modes.slam_left -= dt;
        if modes.slam_left <= 0. {
            modes.slam = None;
        }
    }

    // A mode hosted with its session starts once the session is up.
    if let Some((start, wait)) = modes.hosted.take() {
        // Held while the player names themselves; dropped if they back out.
        let prompt = crate::graphics_menu::name_prompt();
        let wait = if prompt == 1 { wait } else { wait - dt };
        if prompt == 2 {
            crate::graphics_menu::clear_name_prompt();
            return;
        }
        match session {
            Some((_, true, _)) => modes.request = Some(Some(start)),
            Some(_) => {}
            None if wait <= 0. => modes.request = Some(Some(start)),
            None => modes.hosted = Some((start, wait)),
        }
    }
    // Start or stop (the host decides for everyone).
    if let Some(request) = modes.request.take() {
        if !host {
            modes.say("Only the session host can start or end a game mode.", 3.);
        } else {
            match request {
                Some(start) => match begin(&mut modes, start, players.clone(), now ^ (u64::from(skater.scoring.landing_seq) << 17) ^ 0x9e37_79b9) {
                    Ok(()) => sounds.push("mode_start"),
                    Err(error) => modes.say(error, 3.),
                },
                None => {
                    modes.round = None;
                    modes.say("ID_CHALLENGE_CENTRAL_MESSAGE_ROUNDOVER", 2.);
                }
            }
        }
    }

    // Guests follow the host's round.
    if !host {
        let latest = net
            .records(ROUND_KEY)
            .into_iter()
            .filter_map(|(_, value, received)| serde_json::from_slice::<Option<Round>>(&value).ok().map(|r| (r, received)))
            .max_by_key(|(round, _)| round.as_ref().map_or(0, |r| r.id));
        match latest {
            Some((Some(round), received)) => {
                let elapsed = now.saturating_sub(received) as f32 / 1000.;
                modes.left = (round.left_ms as f32 / 1000. - elapsed).max(0.);
                if modes.round.as_ref().is_none_or(|r| r.id != round.id) {
                    modes.seen_turn = 0;
                    modes.seen_outcome = None;
                    sounds.push("mode_start");
                }
                modes.round = Some(round);
            }
            Some((None, _)) | None => {
                if modes.round.take().is_some() {
                    modes.say("ID_CHALLENGE_CENTRAL_MESSAGE_ROUNDOVER", 2.);
                }
            }
        }
    }

    let Some(round) = modes.round.clone() else {
        WAITING.store(false, Ordering::Relaxed);
        *CAMERA.lock().unwrap_or_else(|e| e.into_inner()) = None;
        modes.slam = None;
        return;
    };
    let Some(challenge) = modes.challenge().cloned() else {
        modes.round = None;
        return;
    };
    let me_active = round.kind.simultaneous() || round.active == local;

    // A new turn: the active player to the spawn point, the rest to wait.
    if round.turn != modes.seen_turn && round.state == State::Countdown {
        modes.seen_turn = round.turn;
        modes.mine = Turn { round: round.id, turn: round.turn, ..default() };
        modes.turn_left = challenge.time_limit();
        modes.touched_spot = false;
        modes.touched_objects.clear();
        modes.was_wiping = false;
        modes.seen_leader = None;
        modes.sequences = 0;
        modes.outside_for = 0.;
        modes.bail = None;
        (modes.fall_top, modes.fall_time, modes.fall_speed) = (None, 0., 0.);
        modes.part_velocities.clear();
        modes.landing_seq = skater.scoring.landing_seq;
        let place = if me_active { challenge.spawn(round.round) } else { challenge.waiting(round.round) };
        if let Some(mut matrix) = place.and_then(Place::transform) {
            if round.kind.simultaneous() {
                // Everyone starts at once: side by side on the line.
                let i = round.players.iter().position(|p| p.id == local).unwrap_or(0) as f32;
                let shift = (i - (round.players.len() as f32 - 1.) / 2.) * 1.5;
                for k in 0..3 {
                    matrix[3][k] += matrix[0][k] * shift;
                }
            }
            if let Err(error) = skater.place(matrix) {
                warn!("Game mode: could not place the skater ({error})");
            }
        }
        let active = modes.names.get(&round.active).cloned().unwrap_or_else(|| "Player".into());
        match round.kind {
            Kind::Domination | Kind::Deathrace => {}
            Kind::Skate => {
                let id = if round.set_trick.is_some() { "ID_SKATE_COPY_TRICK" } else { "ID_SKATE_IS_LEADER" };
                let text = modes.strings.format(id, &[active]);
                modes.say_coloured(text, 2.5, if me_active { 2 } else { 0 });
            }
            _ if me_active => {
                let text = if round.kind == Kind::OneUp && round.leader.is_none() {
                    modes.strings.format("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_SETYOURSCORE", &[modes.name(local)])
                } else {
                    modes.strings.get("ID_CHALLENGE_CENTRAL_MESSAGE_YOURTURN")
                };
                modes.say_coloured(text, 2.5, 2);
            }
            _ => {
                let text = modes.strings.format("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_OTHERSTURN", &[modes.name(round.active)]);
                modes.say(text, 2.5);
            }
        }
        info!("GAME_MODE_TURN round={} turn={} active={} me={}", round.round, round.turn, round.active, me_active);
    }
    // Everyone sees each turn's outcome once.
    if round.outcome.is_some() && round.outcome != modes.seen_outcome {
        modes.seen_outcome = round.outcome.clone();
        outcome_message(&mut modes, round.outcome.as_ref().unwrap(), &mut sounds);
    }

    // Waiting players watch: no gameplay, the camera on the active skater.
    let watching = !me_active && matches!(round.state, State::Countdown | State::Running | State::EndTurn);
    WAITING.store(watching, Ordering::Relaxed);
    *CAMERA.lock().unwrap_or_else(|e| e.into_inner()) = watching
        .then(|| net.actor_pose(round.active))
        .flatten()
        .map(|(at, rotation)| {
            let forward = rotation * Vec3::Z;
            let flat = Vec3::new(forward.x, 0., forward.z).normalize_or(Vec3::Z);
            Transform::from_translation(at - flat * 5. + Vec3::Y * 2.5).looking_at(at + Vec3::Y * 0.8, Vec3::Y)
        });

    // The active player's turn.
    if me_active && round.state == State::Running && !modes.mine.done {
        play_turn(&mut modes, &challenge, &round, &skater, output, position, dt, &mut sounds);
    }
    // Domination: a new leader is announced.
    if round.kind == Kind::Domination && round.state == State::Running && round.players.len() > 1 {
        let leader = standings(&modes).first().filter(|r| r.1 > 0.).map(|r| r.0);
        if leader != modes.seen_leader {
            modes.seen_leader = leader;
            if let (Some(id), None) = (leader, &modes.message) {
                let text = if id == local {
                    modes.strings.get("ID_CONTEST_CENTRALMESSAGE_NEWLEADYOU")
                } else {
                    modes.strings.format("ID_CONTEST_MODCENTRALMESSAGE_NEWLEAD", &[modes.name(id)])
                };
                modes.say(text, 2.5);
            }
        }
    }

    // The clock: the host runs it; everyone counts down what was sent.
    modes.left -= dt;
    if host {
        let report = if me_active {
            Some(modes.mine.clone())
        } else {
            net.records(MINE_KEY)
                .into_iter()
                .find(|(id, _, _)| *id == round.active)
                .and_then(|(_, v, _)| serde_json::from_slice::<Turn>(&v).ok())
        };
        let connected: Vec<u64> = players.clone();
        let state = modes.round.as_ref().unwrap().state;
        match state {
            State::Countdown if modes.left <= 0. => {
                modes.round.as_mut().unwrap().state = State::Running;
                modes.left = challenge.time_limit();
            }
            State::Running if round.kind.simultaneous() => {
                // Everyone's progress, folded in as they play.
                let mut reports: Vec<(u64, Turn)> = net
                    .records(MINE_KEY)
                    .into_iter()
                    .filter(|(id, _, _)| *id != local && connected.contains(id))
                    .filter_map(|(id, v, _)| serde_json::from_slice::<Turn>(&v).ok().map(|t| (id, t)))
                    .collect();
                reports.push((local, modes.mine.clone()));
                tally(&mut modes, &reports);
                let r = modes.round.as_ref().unwrap();
                let raced = r.kind == Kind::Deathrace
                    && r.players.iter().filter(|p| connected.contains(&p.id)).all(|p| p.finished.is_some() || p.dnf);
                if raced || modes.left <= 0. {
                    modes.round.as_mut().unwrap().state = State::Finished;
                    modes.left = RESULTS;
                }
            }
            State::Running => {
                let gone = !connected.contains(&round.active);
                let reported = report.as_ref().filter(|r| r.round == round.id && r.turn == round.turn && r.done);
                if let Some(report) = reported {
                    end_turn(&mut modes, &report.clone());
                } else if gone || modes.left <= -TURN_GRACE {
                    end_turn(&mut modes, &Turn { timed_out: true, ..default() });
                }
            }
            State::EndTurn if modes.left <= 0. => next_turn(&mut modes, &connected),
            State::Finished if modes.left <= 0. => {
                modes.round = None;
            }
            _ => {}
        }
    }
    if modes.round.as_ref().is_some_and(|r| r.state == State::Finished) && modes.seen_outcome != Some(Outcome::NotSet { id: u64::MAX }) {
        modes.seen_outcome = Some(Outcome::NotSet { id: u64::MAX });
        finish_message(&mut modes, &mut sounds);
    }

    // Share: the round (host) and this player's turn, twice a second.
    modes.publish_in -= dt;
    if session.is_some() && modes.publish_in <= 0. {
        modes.publish_in = 0.5;
        if let Ok(bytes) = serde_json::to_vec(&modes.mine) {
            net.publish_application(MINE_KEY, bytes);
        }
        if host {
            let left = modes.left;
            let round = modes.round.clone().map(|mut r| {
                r.left_ms = (left.max(0.) * 1000.) as u64;
                r
            });
            if let Ok(bytes) = serde_json::to_vec(&round) {
                net.publish_application(ROUND_KEY, bytes);
            }
        }
    }
}

/// Count this player's turn: sequences in the spot, bails, the clock.
#[allow(clippy::too_many_arguments)]
fn play_turn(
    modes: &mut GameModes,
    challenge: &Challenge,
    round: &Round,
    skater: &crate::physics::SkaterRuntime,
    output: &skate_core::physics::phase::PhysicalOutputSnapshot,
    position: Vec3,
    dt: f32,
    sounds: &mut crate::game_audio::SoundQueue,
) {
    let kind = round.kind;
    modes.turn_left -= dt;
    modes.outside_for = if matches!(kind, Kind::Skate | Kind::Deathrace) && !challenge.in_area(position) {
        modes.outside_for + dt
    } else {
        0.
    };
    // A sequence that began (or passed) inside the spot still scores if it
    // leaves it (the game's ForceEnableScoring).
    if skater.scoring.sequence_active() && challenge.in_spot(position) {
        modes.touched_spot = true;
    }
    if kind == Kind::Domination {
        modes.touched_objects.resize(challenge.objects.len(), false);
        modes.mine.claims.resize(challenge.objects.len(), 0.);
        if skater.scoring.sequence_active() {
            for (touched, object) in modes.touched_objects.iter_mut().zip(&challenge.objects) {
                *touched |= object.contains(position);
            }
        }
    }
    let cumulative = challenge.flag("CumulativeScoring") || kind == Kind::OneUp;
    let landing_seq = skater.scoring.landing_seq;
    let landed = landing_seq != modes.landing_seq;
    let score = skater.scoring.landed_score.max(0.);
    let in_spot = challenge.in_spot(position) || modes.touched_spot;
    if landed {
        modes.landing_seq = landing_seq;
        modes.touched_spot = false;
    }
    let wiped = output.wiping_out && !modes.was_wiping;
    modes.was_wiping = output.wiping_out;
    match kind {
        Kind::SpotBattle | Kind::OneUp => {
            if landed && (kind == Kind::OneUp || in_spot) && score > 0. {
                modes.sequences += 1;
                if cumulative {
                    modes.mine.score += score;
                } else {
                    modes.mine.score = modes.mine.score.max(score);
                }
                sounds.push("mode_score");
                let to_end = challenge.number("NumSequencesToEndTurn", 1.).max(1.) as u32;
                if kind == Kind::SpotBattle && cumulative && modes.sequences >= to_end {
                    modes.mine.done = true;
                }
            }
            if output.wiping_out {
                if kind == Kind::OneUp && !challenge.flag("AllowPlayerBail") && !modes.mine.done {
                    let text = modes.strings.get("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_BAILED");
                    modes.say_coloured(text, 2., 1);
                }
                modes.mine.done = true;
            } else if modes.turn_left <= 0. {
                modes.mine.done = true;
                modes.mine.timed_out = true;
            }
        }
        Kind::HallOfMeat => {
            // The slam scores while the rider is down; the turn ends once it is over.
            let bodies = skater.skeleton.bodies();
            let velocity = |part: usize| {
                let v = &bodies[part].rates.linear_velocity;
                Vec3::new(v.x, v.y, v.z)
            };
            let height = bodies[HIPS].rates.position.y;
            let previous = std::mem::replace(&mut modes.part_velocities, (0..RAGDOLL_PARTS).map(velocity).collect());
            let airborne = output.contact_count == 0 && !skater.collision_feedback.current.iter().any(|&c| c);
            if output.wiping_out {
                let bones = modes.meat.bones.len().max(25);
                let (fall_top, fall_time) = (modes.fall_top.unwrap_or(height).max(height), modes.fall_time);
                let fall_speed = modes.fall_speed;
                let bail = modes.bail.get_or_insert_with(|| Bail {
                    speed: velocity(HIPS).length().max(fall_speed),
                    top: fall_top,
                    bottom: height,
                    air: fall_time,
                    longest_air: fall_time,
                    velocities: if previous.len() == RAGDOLL_PARTS { previous } else { (0..RAGDOLL_PARTS).map(velocity).collect() },
                    impacts: vec![0.; bones],
                    ..default()
                });
                // A part's sudden change of velocity, less what gravity
                // added this tick, is the blow it took.
                for part in 1..RAGDOLL_PARTS {
                    let now = velocity(part);
                    let impact = (now - bail.velocities[part] - Vec3::NEG_Y * 9.81 * dt).length();
                    bail.velocities[part] = now;
                    modes.meat.hit(&mut bail.impacts, part - 1, impact);
                }
                let spin = &bodies[HIPS].rates.angular_velocity;
                bail.rotation += Vec3::new(spin.x, spin.y, spin.z).length().to_degrees() * dt;
                bail.speed = bail.speed.max(velocity(HIPS).length());
                bail.top = bail.top.max(height);
                bail.bottom = bail.bottom.min(height);
                bail.down += dt;
                if !airborne {
                    bail.air = 0.;
                } else {
                    bail.air += dt;
                    bail.longest_air = bail.longest_air.max(bail.air);
                }
                let slam = bail.slam(&modes.meat);
                modes.slam = Some(slam);
                modes.slam_left = 3.;
            } else if let Some(bail) = modes.bail.take() {
                modes.mine.score = modes.mine.score.max(bail.slam(&modes.meat).score as f32);
                modes.mine.done = true;
            } else if modes.turn_left <= 0. {
                modes.mine.done = true;
                modes.mine.timed_out = true;
            }
            // Riding: the fall so far (it counts if it ends in a wipeout).
            if !output.wiping_out {
                // The wipeout follows the touchdown by a moment.
                if airborne {
                    modes.fall_top = Some(modes.fall_top.unwrap_or(height).max(height));
                    modes.fall_time += dt;
                    modes.fall_speed = modes.fall_speed.max(velocity(HIPS).length());
                    modes.grounded = 0.;
                } else {
                    modes.grounded += dt;
                    if modes.grounded > 0.5 {
                        (modes.fall_top, modes.fall_time, modes.fall_speed) = (None, 0., 0.);
                    }
                }
            }
        }
        Kind::Skate => {
            // The first flip trick or grab landed is the attempt; it must be
            // in the spot. Ollies and other tricks don't count.
            let air = skater.scoring.landed_air.clone().filter(|a| landed && SETTABLE.contains(&a.trick_type));
            if let Some(air) = air {
                modes.mine.trick = Some(SetTrick { name: air.name, gesture: gesture(air.identifier) });
                if !in_spot {
                    modes.mine.fail = Some(Fail::OutOfSpot);
                }
                modes.mine.done = true;
            } else if output.wiping_out {
                modes.mine.fail = Some(Fail::Bailed);
                modes.mine.done = true;
            } else if modes.turn_left <= 0. {
                modes.mine.fail = Some(Fail::OutOfTime);
                modes.mine.done = true;
                modes.mine.timed_out = true;
            }
        }
        Kind::Domination => {
            // A sequence in an object beating its owner's takes it.
            if landed && score > 0. {
                let mut increased = false;
                for i in 0..challenge.objects.len() {
                    if (modes.touched_objects[i] || challenge.objects[i].contains(position)) && score > modes.mine.claims[i] {
                        let owner = round.objects.get(i).copied().flatten();
                        increased |= owner.is_some_and(|(id, s)| id == modes.local && score > s);
                        modes.mine.claims[i] = score;
                    }
                }
                if increased {
                    let text = modes.strings.get("ID_DOMINATION_CENTRAL_MESSAGE_SCOREINCREASED");
                    modes.say_coloured(text, 2., 2);
                }
                sounds.push("mode_score");
            }
            if landed || !skater.scoring.sequence_active() {
                modes.touched_objects.iter_mut().for_each(|t| *t = false);
            }
        }
        Kind::Deathrace => {
            let (gates, finish) = challenge.gates();
            let next = modes.mine.gate as usize;
            if next < gates.len() {
                if gates[next].contains(position) {
                    modes.mine.gate += 1;
                    sounds.push("mode_score");
                } else if challenge.gate_skippable {
                    // Skipping ahead counts the gates in between as missed.
                    if let Some(j) = (next + 1..gates.len()).find(|&j| gates[j].contains(position)) {
                        modes.mine.missed += (j - next) as u32;
                        modes.mine.gate = j as u32 + 1;
                        let text = modes.strings.format("ID_RACE_GATES_MISSED", &[modes.mine.missed.to_string()]);
                        modes.say_coloured(text, 2., 1);
                    }
                }
            }
            let through = modes.mine.gate as usize >= gates.len();
            if through && (finish.is_empty() || finish.iter().any(|v| v.contains(position))) {
                modes.mine.finished = Some(challenge.time_limit() - modes.turn_left.max(0.));
                modes.mine.done = true;
                let text = modes.strings.get("ID_RACE_FINISHED");
                modes.say_coloured(text, 3., 2);
                sounds.push("mode_score");
            } else if wiped {
                modes.mine.wipeouts += 1;
                let limit = challenge.wipeout_limit();
                if limit > 0 && modes.mine.wipeouts > limit {
                    modes.mine.dnf = true;
                    modes.mine.done = true;
                }
            }
        }
    }
}

fn outcome_message(modes: &mut GameModes, outcome: &Outcome, sounds: &mut crate::game_audio::SoundQueue) {
    let local = modes.local;
    match *outcome {
        Outcome::Scored { id, score, owned } => {
            if owned {
                let text = modes.strings.get("ID_CHALLENGE_CENTRAL_MESSAGE_SPOTOWNED");
                if id == local {
                    modes.celebrate(text, 3., true);
                } else {
                    modes.say(format!("{} {}", modes.name(id), points(score)), 3.);
                }
            } else {
                modes.say(format!("{}: {}", modes.name(id), points(score)), 2.5);
            }
        }
        Outcome::TimeUp { .. } => {
            let text = modes.strings.get("ID_CHALLENGE_CENTRALMESSAGE_RANOUTOFTIME");
            modes.say_coloured(text, 2.5, 1);
        }
        Outcome::NewScore { id, score, one_upped } => {
            if one_upped && id == local {
                let text = modes.strings.get("ID_CHALLENGE_CENTRAL_MESSAGE_MODIFIED_1UP_ONEUPPED");
                modes.celebrate(text, 3., false);
            } else {
                let text = modes.strings.format("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_NEWSCORESET", &[points(score)]);
                modes.say_coloured(text, 3., if id == local { 2 } else { 0 });
            }
            sounds.push("one_up");
        }
        Outcome::Letter { id, letters, eliminated } => {
            let text = if eliminated {
                modes.strings.format("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_ELIMINATED", &[modes.name(id)])
            } else if id == local {
                modes.strings.get("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_SCOREDLOW")
            } else {
                modes.strings.format("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_OTHER_OUTFORROUND", &[modes.name(id)])
            };
            modes.show_letter(MAX_LETTERS as u32 + 2 + letters as u32);
            modes.queued = Some((text, if id == local { 1 } else { 0 }, LETTERS_TIME));
        }
        Outcome::NotSet { .. } => {
            let text = modes.strings.get("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_SCORENOTSET");
            modes.say(text, 2.5);
        }
        Outcome::SkateSet { id, ref trick } => {
            let text = format!("{} {trick}", modes.strings.get("ID_SKATE_TRICK_SET"));
            modes.say_coloured(text, 3., if id == local { 2 } else { 0 });
            sounds.push("mode_score");
        }
        Outcome::SkateFail { id, fail } => {
            let text = fail_text(modes, id, fail);
            let name = modes.names.get(&id).cloned().unwrap_or_default();
            let next = modes.round.as_ref().and_then(|r| r.leader).map(|l| modes.strings.format("ID_SKATE_NEWLEADER", &[modes.names.get(&l).cloned().unwrap_or(name)]));
            modes.say_coloured(next.map_or(text.clone(), |n| format!("{text} - {n}")), 3., 1);
        }
        Outcome::SkateCopied { id, everyone } => {
            let text = modes.strings.get(if everyone { "ID_CHALLENGE_CENTRAL_MESSAGE_NAMEGETSALETTER" } else { "ID_SKATE_TRICK_COPIED" });
            modes.say_coloured(text, 3., if id == local { 2 } else { 0 });
            sounds.push("mode_score");
        }
        Outcome::SkateMissed { id, letters, eliminated, fail } => {
            let text = if eliminated {
                modes.strings.format("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_ELIMINATED", &[modes.name(id)])
            } else {
                let why = fail.map_or_else(|| modes.strings.get("ID_SKATE_TRICK_NOT_COPIED"), |f| fail_text(modes, id, f));
                format!("{why} - {}", letters_text(Kind::Skate, letters))
            };
            modes.show_letter(letters as u32);
            modes.queued = Some((text, if id == local { 1 } else { 0 }, LETTERS_TIME));
            sounds.push("one_up");
        }
        Outcome::Owned { id, from, .. } => {
            if id == local {
                let text = modes.strings.get("ID_DOMINATION_CENTRAL_MESSAGE_OWNEDSPOT");
                modes.say_coloured(text, 2., 2);
                sounds.push("mode_score");
            } else if from == Some(local) {
                let text = modes.strings.format("ID_DOMINATION_CENTRAL_MESSAGE_LOSTSPOT", &[modes.name(id)]);
                modes.say_coloured(text, 2.5, 1);
            }
        }
        Outcome::Finished { id, position, dnf, .. } => {
            let name = modes.name(id);
            let text = if dnf {
                format!("{name}: {}", modes.strings.get("ID_RACE_DNF"))
            } else if id == local {
                let others = modes.round.as_ref().is_some_and(|r| r.players.iter().any(|p| p.finished.is_none() && !p.dnf));
                let place = modes.strings.format("ID_RACE_POSITION_NAME_SEPERATOR", &[position.to_string(), name]);
                if others { format!("{place} - {}", modes.strings.get("ID_RACE_CENTRAL_MESSAGE_WAITING_FOR_FINISH")) } else { place }
            } else {
                modes.strings.format("ID_RACE_POSITION_NAME_SEPERATOR", &[position.to_string(), name])
            };
            modes.say_coloured(text, 3., if id == local && !dnf { 2 } else { 0 });
        }
    }
}

/// Why a S.K.A.T.E. attempt failed, as the game says it.
fn fail_text(modes: &GameModes, id: u64, fail: Fail) -> String {
    match fail {
        Fail::Bailed => modes.strings.format("ID_SKATE_PLAYER_BAILED", &[modes.name(id)]),
        Fail::OutOfTime => modes.strings.get("ID_SKATE_OUT_OF_TIME"),
        Fail::OutOfSpot => modes.strings.get("ID_CHALLENGE_CENTRAL_MESSAGE_NAMEDIDNOTTRICKVALIDAREA"),
        Fail::LeftArea => modes.strings.get("ID_SKATE_LEFT_AREA"),
        Fail::Repeated => modes.strings.get("ID_SKATE_TRICK_ABD"),
    }
}

/// A player's letters: 1-U-P or S.K.A.T.E. so far.
fn letters_text(kind: Kind, letters: u8) -> String {
    match kind {
        Kind::Skate => SKATE.chars().take(letters as usize).map(|c| format!("{c}.")).collect(),
        _ => "1UP".chars().take(letters as usize).collect(),
    }
}

fn standings(modes: &GameModes) -> Vec<(u64, f32, u8, bool)> {
    let Some(round) = &modes.round else { return vec![] };
    let mut rows: Vec<(u64, f32, u8, bool)> = round.players.iter().map(|p| (p.id, p.score, p.letters, p.eliminated)).collect();
    let player = |id: u64| round.players.iter().find(|p| p.id == id).unwrap();
    match round.kind {
        Kind::OneUp | Kind::Skate => rows.sort_by(|a, b| a.3.cmp(&b.3).then(a.2.cmp(&b.2)).then(b.1.total_cmp(&a.1))),
        Kind::Domination => rows.sort_by(|a, b| b.1.total_cmp(&a.1).then(player(b.0).extra.total_cmp(&player(a.0).extra))),
        Kind::Deathrace => rows.sort_by(|a, b| race_order(player(a.0), player(b.0))),
        _ => rows.sort_by(|a, b| b.1.total_cmp(&a.1)),
    }
    rows
}

impl GameModes {
    /// Whether this player won the round: first in the standings, with a
    /// score (or the last one standing; or across the finish line).
    fn won(&self) -> bool {
        let rows = standings(self);
        let Some(&(winner, score, ..)) = rows.first() else { return false };
        let finished = self.round.as_ref().and_then(|r| r.players.iter().find(|p| p.id == winner)).is_some_and(|p| p.finished.is_some());
        winner == self.local
            && match self.active() {
                Some(Kind::OneUp | Kind::Skate) => true,
                Some(Kind::Deathrace) => finished,
                _ => score > 0.,
            }
    }
}

fn finish_message(modes: &mut GameModes, sounds: &mut crate::game_audio::SoundQueue) {
    let rows = standings(modes);
    let Some(&(winner, ..)) = rows.first() else { return };
    let kind = modes.active();
    let won = modes.won();
    sounds.push(if won { "mode_win" } else { "mode_end" });
    if kind == Some(Kind::Deathrace) && !won {
        let text = modes.strings.get("ID_LEADERBOARD_LOST_RACE");
        modes.say(text, 3.);
    } else if matches!(kind, Some(Kind::OneUp | Kind::Skate)) {
        let text = modes.strings.format("ID_CHALLENGE_CENTRAL_MESSAGE_1UP_WINS", &[modes.name(winner)]);
        if won { modes.celebrate(text, 4., false) } else { modes.say(text, 4.) }
    } else if won {
        let text = modes.strings.get("ID_CHALLENGE_COMMON_KILLED_IT");
        modes.celebrate(text, 3., false);
    } else {
        modes.say("ID_CHALLENGE_CENTRAL_MESSAGE_ROUNDOVER", 3.);
    }
}

/// Opt-in coverage (--verify with SKATE_VERIFY_MODE=<mode>/<spot>[/kickflip]):
/// start that mode at that spot of this map once the skater is ready, and log
/// the turn's place and figures. `kickflip` lands one in the spot on every
/// S.K.A.T.E. attempt, to show the set and copied trick; `gates` carries the
/// skater through a Deathrace's gates, one a second; `drop` drops the skater
/// from 15 m for a Hall of Meat slam; `finish` ends the round early.
fn verify(
    mut modes: ResMut<GameModes>,
    mut skater: ResMut<crate::physics::SkaterRuntime>,
    animation: Res<crate::animation::AnimationStatus>,
    net: Res<crate::multiplayer::Multiplayer>,
    mut started: Local<bool>,
    mut dropped: Local<bool>,
    mut tick: Local<f32>,
    time: Res<Time<Real>>,
) {
    let Ok(spec) = std::env::var("SKATE_VERIFY_MODE") else { return };
    let mut parts = spec.split('/');
    let (mode, spot, script) = (parts.next().unwrap_or(""), parts.next().unwrap_or("0"), parts.next());
    // SKATE_VERIFY_PLAYERS=n: wait for n players in the session first.
    let wanted = std::env::var("SKATE_VERIFY_PLAYERS").ok().and_then(|n| n.parse::<usize>().ok()).unwrap_or(1);
    let joined = if net.session_clock().is_some() { net.player_ids().len() } else { 1 };
    if !*started && animation.ready && modes.loaded && joined >= wanted {
        let Some(kind) = Kind::from_mode(mode) else { return };
        *started = true;
        let spots = modes.spots(kind);
        info!("GAME_MODE_VERIFY map={:?} {kind:?} spots={spots:?}", modes.map);
        modes.request(Some(Start { kind, challenge: spot.parse().ok() }));
    }
    *tick += time.delta_secs();
    if *started && *tick >= 1. {
        *tick = 0.;
        let attempt = modes.round.as_ref().is_some_and(|r| r.kind == Kind::Skate && r.state == State::Running && modes.turn_left < 12.);
        let racing = modes.round.as_ref().is_some_and(|r| r.kind == Kind::Deathrace && r.state == State::Running);
        if script == Some("gates") && racing && !modes.mine.done {
            let challenge = modes.challenge().cloned().unwrap_or_default();
            let (gates, finish) = challenge.gates();
            let next = modes.mine.gate as usize;
            let target = gates.get(next).or(finish.first()).and_then(|v| v.floor());
            let ahead = gates.get(next + 1).or(finish.first()).and_then(|v| v.center).map(Vec3::from_array);
            if let Some(at) = target {
                let _ = skater.travel_to(facing(at, ahead.unwrap_or(at + Vec3::Z)));
            }
            info!("GAME_MODE_VERIFY race gate={} missed={} finished={:?}", modes.mine.gate, modes.mine.missed, modes.mine.finished);
        }
        // `drop`: fall from 15 m above the spot, for a Hall of Meat slam.
        let dropping = modes.round.as_ref().is_some_and(|r| r.state == State::Running) && modes.turn_left < modes.challenge().map_or(0., Challenge::time_limit) - 1.;
        if script == Some("drop") && dropping && modes.bail.is_none() && !*dropped {
            *dropped = true;
            if let Some(mut m) = modes.challenge().and_then(|c| c.spawn(1)).and_then(Place::transform) {
                // Upside down, so it is a slam, not a landing.
                m[3][1] += 15.;
                for k in 0..3 {
                    m[1][k] = -m[1][k];
                    m[2][k] = -m[2][k];
                }
                let _ = skater.travel_to(m);
            }
        }
        if let Some(slam) = &modes.slam {
            info!("GAME_MODE_VERIFY slam score={} metrics={:?} bonuses={:?}", slam.score, slam.metrics, slam.scores);
        }
        // `finish`: end the round after a few seconds, for the results.
        if script == Some("finish") && modes.round.as_ref().is_some_and(|r| r.state == State::Running) && modes.left < modes.challenge().map_or(0., Challenge::time_limit) - 3. {
            modes.round.as_mut().unwrap().state = State::Finished;
            modes.left = RESULTS;
        }
        if matches!(script, Some("kickflip" | "miss")) && attempt && !modes.mine.done {
            // `miss`: set a kickflip, then miss every copy (a letter each).
            let copying = modes.round.as_ref().is_some_and(|r| r.set_trick.is_some());
            if script == Some("miss") && copying {
                modes.mine.fail = Some(Fail::Bailed);
            } else {
                modes.mine.trick = Some(SetTrick { name: "Kickflip".into(), gesture: gesture("kickflip") });
            }
            modes.mine.done = true;
        }
        if let Some(round) = &modes.round {
            info!("GAME_MODE_VERIFY outcome={:?} set={:?}", round.outcome, round.set_trick.as_ref().map(|t| &t.name));
            info!("GAME_MODE_VERIFY state={:?} round={} turn={} left={:.1} turn_left={:.1} score={} done={}", round.state, round.round, round.turn, modes.left, modes.turn_left, modes.mine.score, modes.mine.done);
        }
    }
}

/// Where to go: the challenge's volumes outlined in the world (the game
/// drew its own gate ribbons and spot markers; those models aren't ported).
/// Deathrace shows the next gates, Domination each object by owner, the
/// turn-based modes their spot.
fn guides(modes: Res<GameModes>, mut gizmos: Gizmos) {
    let (Some(round), Some(challenge)) = (&modes.round, modes.challenge()) else { return };
    if round.state == State::Finished {
        return;
    }
    let mut outline = |v: &Volume, color: Color| {
        let (Some(b), Some(c), Some(h)) = (v.basis, v.center, v.half) else { return };
        let axes: [Vec3; 3] = std::array::from_fn(|i| Vec3::new(b[i * 3], b[i * 3 + 1], b[i * 3 + 2]) * h[i]);
        let corner = |x: f32, y: f32, z: f32| Vec3::from_array(c) + axes[0] * x + axes[1] * y + axes[2] * z;
        for (a, d) in [(0, 1), (1, 2), (2, 0)] {
            for (s, t) in [(-1., -1.), (-1., 1.), (1., -1.), (1., 1.)] {
                let mut p = [0.; 3];
                p[a] = s;
                p[d] = t;
                let free = 3 - a - d;
                let (mut lo, mut hi) = (p, p);
                lo[free] = -1.;
                hi[free] = 1.;
                gizmos.line(corner(lo[0], lo[1], lo[2]), corner(hi[0], hi[1], hi[2]), color);
            }
        }
    };
    match round.kind {
        Kind::Deathrace => {
            let (gates, finish) = challenge.gates();
            let next = modes.mine.gate as usize;
            for (i, gate) in gates.iter().enumerate().skip(next).take(2) {
                outline(gate, if i == next { Color::srgb(1., 0.85, 0.1) } else { Color::srgba(1., 0.85, 0.1, 0.35) });
            }
            if next + 1 >= gates.len() {
                finish.iter().for_each(|v| outline(v, Color::srgb(1., 1., 1.)));
            }
        }
        Kind::Domination => {
            for (i, object) in challenge.objects.iter().enumerate() {
                let color = match round.objects.get(i).copied().flatten() {
                    Some((id, _)) if id == modes.local => Color::srgb(0.2, 1., 0.3),
                    Some(_) => Color::srgb(1., 0.25, 0.2),
                    None => Color::srgb(0.9, 0.9, 0.9),
                };
                outline(object, color);
            }
        }
        Kind::HallOfMeat => {}
        _ => challenge.spot_volumes.iter().for_each(|v| outline(v, Color::srgba(0.3, 0.8, 1., 0.6))),
    }
}

pub(crate) struct GameModesPlugin;
impl Plugin for GameModesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameModes>()
            .add_systems(FixedUpdate, play.after(crate::app::SimulationSet::Physics))
            .add_systems(Update, (verify, guides));
    }
}

// --- HUD ---------------------------------------------------------------------

impl GameModes {
    /// What the original challenge screens show (see `mode_hud`).
    pub(crate) fn view(&self) -> crate::mode_hud::View {
        use crate::mode_hud::{Goal, Objective, Results, Row, View};
        let mut view = View {
            message: self.message.as_ref().map(|(text, _, colour)| (text.clone(), *colour)),
            stinger: self.stinger,
            ..default()
        };
        let (Some(round), Some(challenge)) = (&self.round, self.challenge()) else { return view };
        view.title = self.title(challenge);
        // 1-Up and S.K.A.T.E. standings draw each player's letters.
        view.one_up = round.kind == Kind::OneUp;
        view.skate = round.kind == Kind::Skate;
        view.letter = self.letter;
        view.regular = self.regular;
        let max_letters = if round.kind == Kind::Skate { challenge_letters(Some(challenge)) } else { MAX_LETTERS };
        let (gates, _) = challenge.gates();
        let rows: Vec<Row> = standings(self)
            .into_iter()
            .map(|(id, score, letters, eliminated)| {
                let player = round.players.iter().find(|p| p.id == id);
                let letters_board = matches!(round.kind, Kind::OneUp | Kind::Skate);
                let score = if letters_board { (if eliminated { max_letters } else { letters }) as f32 } else { score };
                let label = match round.kind {
                    // Deathrace: the finish time, else the gates so far.
                    Kind::Deathrace => player.map(|p| match p.finished {
                        _ if p.dnf => self.strings.get("ID_RACE_DNF"),
                        Some(t) => format!("{}:{:05.2}", (t / 60.) as u32, t % 60.),
                        None => format!("{}/{}", p.extra as u32, gates.len()),
                    }),
                    _ => None,
                };
                Row { name: self.name(id), score: score as f64, local: id == self.local, label }
            })
            .collect();
        match round.state {
            State::Countdown => {
                if self.left <= COUNTDOWN {
                    view.countdown = Some(self.left.ceil().clamp(1., 3.) as u32);
                }
            }
            State::Running => {
                let left = if round.active == self.local { self.turn_left } else { self.left };
                let s = left.max(0.).ceil() as i64;
                view.clock = Some((format!("{}:{:02}", s / 60, s % 60), s <= 5));
                if challenge.time_limit() - left < 1. {
                    view.countdown = Some(0);
                }
            }
            State::EndTurn => {}
            State::Finished => {
                let winner = rows.first().cloned();
                let won = self.won();
                view.results = Some(Results {
                    title: "ID_CHALLENGE_COMMON_ROUND_RESULTS".into(),
                    icon: if won { 2 } else { 0 },
                    result: if won { "ID_CHALLENGE_COMMON_KILLED_IT".into() } else { "ID_CHALLENGE_CENTRAL_MESSAGE_ROUNDOVER".into() },
                    detail: winner.map_or(String::new(), |w| format!("{} {}", w.name, w.label.clone().unwrap_or_else(|| points(w.score as f32)))),
                    rows,
                });
                return view;
            }
        }
        // The turn's own figures, for the active player.
        if (round.kind.simultaneous() || round.active == self.local) && matches!(round.state, State::Running | State::EndTurn) {
            let objective = |title: &str, value: String| Objective { state: Goal::Active, title: title.into(), value };
            view.objectives = match round.kind {
                Kind::Skate => vec![],
                Kind::Domination => {
                    let owned = round.objects.iter().flatten().filter(|(o, _)| *o == self.local).count();
                    vec![objective("Spots owned", format!("{owned}/{}", round.objects.len()))]
                }
                Kind::Deathrace => {
                    let mut rows = vec![objective("Gates", format!("{}/{}", self.mine.gate, gates.len()))];
                    if self.mine.missed > 0 {
                        let missed = self.strings.format("ID_RACE_GATES_MISSED_COUNTER", &[self.mine.missed.to_string(), gates.len().to_string()]);
                        rows.push(Objective { state: Goal::Fail, title: missed, value: String::new() });
                    }
                    rows
                }
                // Spot Battle: the DPO panel instead (below).
                Kind::SpotBattle => vec![],
                Kind::HallOfMeat => vec![objective("Best slam", points(self.mine.score))],
                // 1-Up: the DPO panel instead (below).
                Kind::OneUp => vec![],
            };
        }
        // As in Skate 3: sessions show the leaderboard, solo the objectives.
        if round.players.len() > 1 {
            view.players = rows;
            view.objectives.clear();
        }
        view.slam = self.slam.clone();
        // Spot Battle: this turn's score over the best so far (the game's
        // DPOSetParams(TurnScore, leader's Score) under ID_COMMON_SCORE).
        if round.kind == Kind::SpotBattle && round.active == self.local && matches!(round.state, State::Running | State::EndTurn) {
            let best = round.players.iter().map(|p| p.score).fold(0., f32::max);
            view.dpo = Some(crate::mode_hud::Dpo {
                title: "ID_COMMON_SCORE".into(),
                value: self.mine.score as f64,
                target: points(best),
                highlight: if best <= 0. { 1 } else if self.mine.score > best { 2 } else { 0 },
            });
        }
        // 1-Up: this run over the score to beat, under the leader's name.
        if round.kind == Kind::OneUp && round.active == self.local && matches!(round.state, State::Running | State::EndTurn) {
            view.dpo = Some(crate::mode_hud::Dpo {
                title: match round.leader {
                    Some(id) => format!("#{}", self.names.get(&id).cloned().unwrap_or_else(|| self.name(id))),
                    None => "ID_1UP_SET_A_SCORE".into(),
                },
                value: self.mine.score as f64,
                target: if round.leader.is_some() { points(round.best) } else { String::new() },
                highlight: match round.leader {
                    Some(_) if self.mine.score > round.best => 2,
                    Some(_) => 0,
                    None => 1,
                },
            });
        }
        // S.K.A.T.E.: hudabd's trick to set or copy and the tricks already done.
        if round.kind == Kind::Skate && matches!(round.state, State::Countdown | State::Running | State::EndTurn) {
            let trick = |t: &SetTrick| (t.name.clone(), t.gesture.clone());
            view.abd = Some(crate::mode_hud::Abd {
                list: round.abd.iter().rev().take(5).map(|t| t.name.clone()).collect(),
                set: round.set_trick.as_ref().map(trick),
                copy: (round.state == State::EndTurn).then_some(round.attempt.as_ref()).flatten().map(trick),
            });
        }
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(center: [f32; 3], half: [f32; 3], yaw: f32) -> Volume {
        let (s, c) = yaw.sin_cos();
        Volume { name: "v".into(), basis: Some([c, 0., -s, 0., 1., 0., s, 0., c]), center: Some(center), half: Some(half) }
    }

    #[test]
    fn oriented_boxes_and_areas() {
        let v = volume([10., 0., 10.], [4., 2., 1.], std::f32::consts::FRAC_PI_2);
        // Turned a quarter: its long axis lies along z.
        assert!(v.contains(Vec3::new(10., 0., 13.5)));
        assert!(!v.contains(Vec3::new(13.5, 0., 10.)));
        let c = Challenge {
            id: "x".into(), mode: "spot_battle".into(),
            spot_volumes: vec![v], boundary: Some(vec![[0., 0.], [20., 0.], [20., 20.], [0., 20.]]),
            ..default()
        };
        assert!(c.in_spot(Vec3::new(10., 1., 12.)));
        assert!(!c.in_spot(Vec3::new(1., 1., 1.)));
        assert!(c.in_area(Vec3::new(1., 50., 1.)));
        assert!(!c.in_area(Vec3::new(-1., 0., 1.)));
    }

    #[test]
    fn strings_format_like_the_game() {
        let s = Strings([("A".to_string(), "{0:s} Wins! {1:points}^x".to_string())].into_iter().collect());
        assert_eq!(s.format("A", &["Kim".into(), "1,200".into()]), "Kim Wins! 1,200 x");
        assert_eq!(s.get("ID_MISSING"), "ID_MISSING");
    }

    fn round(kind: Kind, players: &[u64]) -> GameModes {
        let challenge: Challenge = serde_json::from_value(serde_json::json!({
            "id": "c", "mode": match kind { Kind::OneUp => "one_up", Kind::HallOfMeat => "hall_of_meat", Kind::Skate => "skate",
                Kind::Domination => "domination", Kind::Deathrace => "deathrace", _ => "spot_battle" },
            "objects": [{"name": "a"}, {"name": "b"}],
            "district": "Map", "settings": {"NumRounds": 2, "TimeLimit": 20.0},
            "spawn": {"name": "s", "matrix": [1.0,0.0,0.0,0.0, 0.0,1.0,0.0,0.0, 0.0,0.0,1.0,0.0, 0.0,0.0,0.0,1.0]}
        })).unwrap();
        let mut modes = GameModes { catalog: vec![challenge], loaded: true, map: Some("Map".into()), ..default() };
        begin(&mut modes, Start { kind, challenge: Some(0) }, players.to_vec(), 0).unwrap();
        modes
    }

    #[test]
    fn spot_battle_turns_and_rounds() {
        let mut modes = round(Kind::SpotBattle, &[1, 2]);
        let mut seen = vec![];
        for turn in 0..10 {
            let active = modes.round.as_ref().unwrap().active;
            seen.push(active);
            end_turn(&mut modes, &Turn { score: 100. * (turn + 1) as f32, done: true, ..default() });
            next_turn(&mut modes, &[1, 2]);
            if modes.round.as_ref().unwrap().state == State::Finished {
                break;
            }
        }
        // Two rounds of two turns, alternating.
        assert_eq!(seen, vec![1, 2, 1, 2]);
        let best: Vec<f32> = modes.round.as_ref().unwrap().players.iter().map(|p| p.score).collect();
        assert_eq!(best, vec![300., 400.]);
    }

    #[test]
    fn one_up_letters_and_the_last_skater_standing() {
        let mut modes = round(Kind::OneUp, &[1, 2]);
        // Player 1 sets 500; player 2 fails (letter) and the round is over.
        let mut log = vec![];
        for _ in 0..20 {
            let r = modes.round.as_ref().unwrap();
            if r.state == State::Finished {
                break;
            }
            let active = r.active;
            let score = if active == 1 { 500. } else { 100. };
            end_turn(&mut modes, &Turn { score, done: true, ..default() });
            log.push((active, modes.round.as_ref().unwrap().outcome.clone()));
            next_turn(&mut modes, &[1, 2]);
        }
        let r = modes.round.as_ref().unwrap();
        assert_eq!(r.state, State::Finished);
        let two = r.players.iter().find(|p| p.id == 2).unwrap();
        assert_eq!((two.letters, two.eliminated), (3, true));
        assert!(log.iter().any(|(_, o)| matches!(o, Some(Outcome::NewScore { id: 1, .. }))));
    }

    fn trick(name: &str) -> Option<SetTrick> {
        Some(SetTrick { name: name.into(), gesture: "ollie_kickflip".into() })
    }

    #[test]
    fn skate_sets_copies_letters_and_passes_the_lead() {
        let mut modes = round(Kind::Skate, &[1, 2, 3]);
        let r = modes.round.as_ref().unwrap();
        assert_eq!((r.active, r.leader), (1, Some(1)));
        // 1 sets a kickflip; 2 copies it, 3 misses (a letter).
        end_turn(&mut modes, &Turn { trick: trick("Kickflip"), done: true, ..default() });
        assert!(matches!(modes.round.as_ref().unwrap().outcome, Some(Outcome::SkateSet { id: 1, .. })));
        next_turn(&mut modes, &[1, 2, 3]);
        assert_eq!(modes.round.as_ref().unwrap().active, 2);
        end_turn(&mut modes, &Turn { trick: trick("Kickflip"), done: true, ..default() });
        assert_eq!(modes.round.as_ref().unwrap().outcome, Some(Outcome::SkateCopied { id: 2, everyone: false }));
        next_turn(&mut modes, &[1, 2, 3]);
        assert_eq!(modes.round.as_ref().unwrap().active, 3);
        end_turn(&mut modes, &Turn { trick: trick("Heelflip"), done: true, ..default() });
        assert!(matches!(modes.round.as_ref().unwrap().outcome, Some(Outcome::SkateMissed { id: 3, letters: 1, eliminated: false, .. })));
        // Everyone has tried: 2 leads and sets.
        next_turn(&mut modes, &[1, 2, 3]);
        let r = modes.round.as_ref().unwrap();
        assert_eq!((r.active, r.leader, r.set_trick.clone()), (2, Some(2), None));
        // A trick already done can't be set: the lead passes to 3, no letter.
        end_turn(&mut modes, &Turn { trick: trick("Kickflip"), done: true, ..default() });
        assert_eq!(modes.round.as_ref().unwrap().outcome, Some(Outcome::SkateFail { id: 2, fail: Fail::Repeated }));
        next_turn(&mut modes, &[1, 2, 3]);
        let r = modes.round.as_ref().unwrap();
        assert_eq!((r.active, r.leader), (3, Some(3)));
        assert_eq!(r.players.iter().map(|p| p.letters).collect::<Vec<_>>(), vec![0, 0, 1]);
        // 3 sets; 1 and 2 bail every copy until two spell S.K.A.T.E.
        for _ in 0..40 {
            let r = modes.round.as_ref().unwrap();
            if r.state == State::Finished {
                break;
            }
            let report = if r.set_trick.is_none() {
                Turn { trick: trick(&format!("Trick {}", r.turn)), done: true, ..default() }
            } else if r.active == 3 {
                Turn { trick: r.set_trick.clone(), done: true, ..default() }
            } else {
                Turn { fail: Some(Fail::Bailed), done: true, ..default() }
            };
            end_turn(&mut modes, &report);
            next_turn(&mut modes, &[1, 2, 3]);
        }
        let r = modes.round.as_ref().unwrap();
        assert_eq!(r.state, State::Finished);
        assert_eq!(standings(&modes)[0].0, 3);
        assert!(r.players.iter().filter(|p| p.id != 3).all(|p| p.letters == 5 && p.eliminated));
    }

    #[test]
    fn domination_owns_by_the_best_sequence() {
        let mut modes = round(Kind::Domination, &[1, 2]);
        let id = modes.round.as_ref().unwrap().id;
        let claims = |a: f32, b: f32| Turn { round: id, claims: vec![a, b], ..default() };
        tally(&mut modes, &[(1, claims(500., 0.)), (2, claims(0., 300.))]);
        let r = modes.round.as_ref().unwrap();
        assert_eq!(r.objects, vec![Some((1, 500.)), Some((2, 300.))]);
        // 2 jacks object 0 and owns both.
        tally(&mut modes, &[(1, claims(500., 0.)), (2, claims(800., 300.))]);
        let r = modes.round.as_ref().unwrap();
        assert!(matches!(r.outcome, Some(Outcome::Owned { id: 2, object: 0, from: Some(1), .. })));
        assert_eq!(standings(&modes)[0], (2, 2., 0, false));
        assert_eq!(r.players.iter().find(|p| p.id == 2).unwrap().extra, 1100.);
    }

    #[test]
    fn deathrace_places_by_finish_then_gates() {
        let mut modes = round(Kind::Deathrace, &[1, 2, 3]);
        let id = modes.round.as_ref().unwrap().id;
        let run = |gate: u32, finished: Option<f32>| Turn { round: id, gate, finished, ..default() };
        tally(&mut modes, &[(1, run(4, None)), (2, run(9, Some(61.5))), (3, run(6, None))]);
        assert!(matches!(modes.round.as_ref().unwrap().outcome, Some(Outcome::Finished { id: 2, position: 1, dnf: false, .. })));
        let order: Vec<(u64, f32)> = standings(&modes).iter().map(|r| (r.0, r.1)).collect();
        // Points: the competitors behind you.
        assert_eq!(order, vec![(2, 2.), (3, 1.), (1, 0.)]);
    }

    #[test]
    fn hall_of_meat_scores_bones_and_bonuses_from_the_tables() {
        let tables: MeatTables = serde_json::from_value(serde_json::json!({
            "curves": {"DropHeightScore": {"x": [0.0, 7.0, 7.0, 10.5, 17.5, 30.0, 40.0, 45.0], "y": [0, 0, 750, 1250, 2500, 5000, 10000, 10000]}},
            "bones": [
                {"spread": [[1, 0.25]], "levels": [[100, 6.0], [250, 9.0], [500, 12.0], [750, 15.0], [0, 0.0], [1000, 18.0]]},
                {"spread": [], "levels": [[100, 9.0], [250, 12.0], [0, 0.0], [0, 0.0], [1000, 21.0], [0, 0.0]]}
            ]
        })).unwrap();
        // Below the first point, on a step, between points, past the end.
        let drop = |m: f32| tables.curve("DropHeightScore", m);
        assert_eq!((drop(5.), drop(14.), drop(60.)), (0., 1875., 10000.));
        assert!((drop(7.001) - 750.).abs() < 1.);
        // A 13 m/s blow sprains bone 0 (500); a quarter of it (3.25) does
        // nothing to bone 1; a 40 m/s blow breaks bone 0 and its quarter
        // (10) scrapes bone 1.
        let mut impacts = vec![0.; 2];
        tables.hit(&mut impacts, 0, 13.);
        assert_eq!((tables.bone_points(0, impacts[0]), tables.bone_points(1, impacts[1])), (500., 0.));
        tables.hit(&mut impacts, 0, 40.);
        assert_eq!((tables.bone_points(0, impacts[0]), tables.bone_points(1, impacts[1])), (1000., 100.));
        tables.hit(&mut impacts, 1, 22.);
        let bail = Bail { top: 14., impacts, ..default() };
        // Body 1000 + 1000, and a 14 m drop.
        assert_eq!(bail.slam(&tables).score, 2000. + 1875.);
    }

    #[test]
    fn flip_tricks_have_their_gestures() {
        assert_eq!(gesture("kickflip"), "ollie_kickflip");
        assert_eq!(gesture("n_heelflip3"), "nollie_heelflip");
        assert_eq!(gesture("360flip"), "ollie_360_flip");
        assert_eq!(gesture("n_varialkickflip"), "nollie_varial_flip");
        assert_eq!(gesture("fs360popshuvit"), "ollie_fs_360popshuvit");
        assert_eq!(gesture("mutegrab"), "trick");
    }
}
