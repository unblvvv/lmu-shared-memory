use std::{cmp::Ordering, collections::HashMap, mem::size_of};

mod shared_memory;

pub use shared_memory::{SharedMemory, SharedMemoryGuard, SharedMemoryLock};

pub const MAPPING_NAME: &str = "LMU_Data";
pub const MAPPING_SIZE: usize = 324_820;
pub const MAX_VEHICLES: usize = 104;

const MAX_READ_RETRIES: usize = 5;
const STARTUP_EVENT: usize = 8;
const SHUTDOWN_EVENT: usize = 12;
const START_SESSION_EVENT: usize = 24;
const SCORING_EVENT: usize = 40;
const TELEMETRY_EVENT: usize = 44;
const APPLICATION_WINDOW: usize = 72;
const SCORING_INFO: usize = 1_632;
const SCORING_TRACK_NAME: usize = SCORING_INFO;
const SCORING_SESSION: usize = SCORING_INFO + 64;
const SCORING_CURRENT_ET: usize = SCORING_INFO + 68;
const SCORING_NUM_VEHICLES: usize = SCORING_INFO + 104;
const SCORING_PLAYER_NAME: usize = SCORING_INFO + 116;
const SCORING_CLOUD_DARKNESS: usize = SCORING_INFO + 212;
const SCORING_RAIN_INTENSITY: usize = SCORING_INFO + 220;
const SCORING_AMBIENT_TEMPERATURE: usize = SCORING_INFO + 228;
const SCORING_TRACK_TEMPERATURE: usize = SCORING_INFO + 236;
const SCORING_WIND: usize = SCORING_INFO + 244;
const SCORING_MIN_PATH_WETNESS: usize = SCORING_INFO + 268;
const SCORING_MAX_PATH_WETNESS: usize = SCORING_INFO + 276;
const SCORING_AVERAGE_PATH_WETNESS: usize = SCORING_INFO + 332;
const VEHICLE_SCORING: usize = 2_192;
const VEHICLE_SCORING_SIZE: usize = 584;
const ACTIVE_TELEMETRY_VEHICLES: usize = 128_464;
const PLAYER_TELEMETRY_INDEX: usize = 128_465;
const PLAYER_HAS_TELEMETRY: usize = 128_466;
const VEHICLE_TELEMETRY: usize = 128_468;
const VEHICLE_TELEMETRY_SIZE: usize = 1_888;

const _: () = assert!(VEHICLE_TELEMETRY + VEHICLE_TELEMETRY_SIZE * MAX_VEHICLES == MAPPING_SIZE);
const _: () = assert!(SCORING_AVERAGE_PATH_WETNESS + size_of::<f64>() <= VEHICLE_SCORING);

#[derive(Debug)]
pub enum Error {
    UnsupportedPlatform,
    SharedMemoryUnavailable {
        name: String,
        source: String,
    },
    MapViewFailed {
        name: String,
    },
    OutOfBounds {
        name: String,
        offset: usize,
        value_size: usize,
        mapping_size: usize,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                write!(f, "LMU shared memory is only available on Windows")
            }
            Self::SharedMemoryUnavailable { name, source } => {
                write!(f, "shared memory '{name}' is unavailable: {source}")
            }
            Self::MapViewFailed { name } => write!(f, "failed to map shared memory '{name}'"),
            Self::OutOfBounds {
                name,
                offset,
                value_size,
                mapping_size,
            } => write!(
                f,
                "read outside shared memory '{name}': {offset} + {value_size} > {mapping_size}"
            ),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

trait Memory {
    fn read<T: Copy>(&self, offset: usize) -> Result<T>;
}

impl Memory for SharedMemory {
    fn read<T: Copy>(&self, offset: usize) -> Result<T> {
        self.read(offset)
    }
}

impl Memory for [u8] {
    fn read<T: Copy>(&self, offset: usize) -> Result<T> {
        let value_size = std::mem::size_of::<T>();
        if offset
            .checked_add(value_size)
            .is_none_or(|end| end > self.len())
        {
            return Err(Error::OutOfBounds {
                name: MAPPING_NAME.to_owned(),
                offset,
                value_size,
                mapping_size: self.len(),
            });
        }
        let pointer = unsafe { self.as_ptr().add(offset).cast::<T>() };
        Ok(unsafe { pointer.read_unaligned() })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PedalData {
    pub throttle: f32,
    pub brake: f32,
    pub clutch: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TelemetrySnapshot {
    pub pedals: PedalData,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TelemetryUpdate {
    Snapshot(TelemetrySnapshot),
    NoPlayer,
    SourceAlive,
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TelemetryHeader {
    event: u32,
    active: u8,
    player_index: u8,
    has_player: u8,
}

#[derive(Default)]
struct TelemetryState {
    last_event: Option<u32>,
    last_elapsed: Option<u64>,
    had_player: bool,
}

pub struct LmuTelemetry {
    mapping: SharedMemory,
    lock: SharedMemoryLock,
    state: TelemetryState,
}

impl LmuTelemetry {
    pub fn connect() -> Result<Self> {
        Ok(Self {
            mapping: SharedMemory::open(MAPPING_NAME, MAPPING_SIZE)?,
            lock: SharedMemoryLock::open()?,
            state: TelemetryState::default(),
        })
    }

    pub fn read(&mut self) -> Result<TelemetryUpdate> {
        let Some(_guard) = self.lock.try_lock() else {
            return Ok(TelemetryUpdate::Unchanged);
        };
        read_telemetry(&self.mapping, &mut self.state)
    }
}

fn read_telemetry<M: Memory + ?Sized>(
    memory: &M,
    state: &mut TelemetryState,
) -> Result<TelemetryUpdate> {
    for _ in 0..MAX_READ_RETRIES {
        let before = telemetry_header(memory)?;
        let player = usize::from(before.player_index);
        let base =
            if before.has_player != 0 && player < usize::from(before.active).min(MAX_VEHICLES) {
                Some(VEHICLE_TELEMETRY + player * VEHICLE_TELEMETRY_SIZE)
            } else {
                None
            };
        let Some(base) = base else {
            let after = telemetry_header(memory)?;
            if before != after {
                continue;
            }
            let changed = state.last_event != Some(after.event) || state.had_player;
            state.last_event = Some(after.event);
            state.last_elapsed = None;
            state.had_player = false;
            return Ok(if changed {
                TelemetryUpdate::NoPlayer
            } else {
                TelemetryUpdate::Unchanged
            });
        };
        let elapsed_before = memory.read::<f64>(base + 12)?.to_bits();
        let throttle = memory.read::<f64>(base + 388)?;
        let brake = memory.read::<f64>(base + 396)?;
        let clutch = memory.read::<f64>(base + 412)?;
        let elapsed_after = memory.read::<f64>(base + 12)?.to_bits();
        let after = telemetry_header(memory)?;
        if before != after || elapsed_before != elapsed_after {
            continue;
        }
        let first = state.last_event.is_none();
        let source_updated = state.last_event != Some(after.event);
        state.last_event = Some(after.event);
        state.had_player = true;
        if first || state.last_elapsed == Some(elapsed_after) {
            state.last_elapsed = Some(elapsed_after);
            return Ok(if source_updated || first {
                TelemetryUpdate::SourceAlive
            } else {
                TelemetryUpdate::Unchanged
            });
        }
        state.last_elapsed = Some(elapsed_after);
        return Ok(TelemetryUpdate::Snapshot(TelemetrySnapshot {
            pedals: PedalData {
                throttle: normalize_input(throttle),
                brake: normalize_input(brake),
                clutch: normalize_input(clutch),
            },
        }));
    }
    Ok(TelemetryUpdate::Unchanged)
}

fn telemetry_header<M: Memory + ?Sized>(memory: &M) -> Result<TelemetryHeader> {
    Ok(TelemetryHeader {
        event: memory.read(TELEMETRY_EVENT)?,
        active: memory.read(ACTIVE_TELEMETRY_VEHICLES)?,
        player_index: memory.read(PLAYER_TELEMETRY_INDEX)?,
        has_player: memory.read(PLAYER_HAS_TELEMETRY)?,
    })
}

fn normalize_input(value: f64) -> f32 {
    if value.is_finite() {
        value.clamp(0., 1.) as f32
    } else {
        0.
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wind {
    /// Wind velocity along LMU's world X axis, in meters per second.
    pub x: f64,
    /// Wind velocity along LMU's world Y axis, in meters per second.
    pub y: f64,
    /// Wind velocity along LMU's world Z axis, in meters per second.
    pub z: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WeatherSnapshot {
    /// Cloud darkness, from 0.0 (clear) to 1.0 (dark).
    pub cloud_darkness: f64,
    /// Rain intensity, from 0.0 (dry) to 1.0 (heavy rain).
    pub rain_intensity: f64,
    /// Ambient temperature in Celsius.
    pub ambient_temperature_celsius: f64,
    /// Track temperature in Celsius.
    pub track_temperature_celsius: f64,
    /// Wind velocity in LMU world coordinates, in meters per second.
    pub wind: Wind,
    /// Lowest wetness on the racing path, from 0.0 (dry) to 1.0 (fully wet).
    pub minimum_path_wetness: f64,
    /// Highest wetness on the racing path, from 0.0 (dry) to 1.0 (fully wet).
    pub maximum_path_wetness: f64,
    /// Average wetness on the racing path, from 0.0 (dry) to 1.0 (fully wet).
    pub average_path_wetness: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WeatherUpdate {
    Snapshot(WeatherSnapshot),
    NoSession,
    Unchanged,
}

#[derive(Default)]
struct WeatherState {
    previous: Option<WeatherSnapshot>,
    had_source: bool,
}

pub struct LmuWeather {
    mapping: SharedMemory,
    lock: SharedMemoryLock,
    state: WeatherState,
}

impl LmuWeather {
    pub fn connect() -> Result<Self> {
        Ok(Self {
            mapping: SharedMemory::open(MAPPING_NAME, MAPPING_SIZE)?,
            lock: SharedMemoryLock::open()?,
            state: WeatherState::default(),
        })
    }

    pub fn read(&mut self) -> Result<WeatherUpdate> {
        let Some(guard) = self.lock.try_lock() else {
            let window: u64 = self.mapping.read(APPLICATION_WINDOW)?;
            if !shared_memory::source_window_alive(window) {
                self.state.had_source = false;
                self.state.previous = None;
                return Err(Error::SharedMemoryUnavailable {
                    name: MAPPING_NAME.into(),
                    source: "source stopped while the shared lock was busy".into(),
                });
            }
            return Ok(WeatherUpdate::Unchanged);
        };
        let frame = read_weather_frame(&self.mapping)?;
        drop(guard);
        decode_weather(frame, &mut self.state)
    }
}

struct RawWeatherFrame {
    startup: u32,
    shutdown: u32,
    window: u64,
    snapshot: WeatherSnapshot,
}

fn read_weather_frame<M: Memory + ?Sized>(memory: &M) -> Result<RawWeatherFrame> {
    Ok(RawWeatherFrame {
        startup: memory.read(STARTUP_EVENT)?,
        shutdown: memory.read(SHUTDOWN_EVENT)?,
        window: memory.read(APPLICATION_WINDOW)?,
        snapshot: WeatherSnapshot {
            cloud_darkness: memory.read(SCORING_CLOUD_DARKNESS)?,
            rain_intensity: memory.read(SCORING_RAIN_INTENSITY)?,
            ambient_temperature_celsius: memory.read(SCORING_AMBIENT_TEMPERATURE)?,
            track_temperature_celsius: memory.read(SCORING_TRACK_TEMPERATURE)?,
            wind: Wind {
                x: memory.read(SCORING_WIND)?,
                y: memory.read(SCORING_WIND + size_of::<f64>())?,
                z: memory.read(SCORING_WIND + size_of::<f64>() * 2)?,
            },
            minimum_path_wetness: memory.read(SCORING_MIN_PATH_WETNESS)?,
            maximum_path_wetness: memory.read(SCORING_MAX_PATH_WETNESS)?,
            average_path_wetness: memory.read(SCORING_AVERAGE_PATH_WETNESS)?,
        },
    })
}

fn decode_weather(frame: RawWeatherFrame, state: &mut WeatherState) -> Result<WeatherUpdate> {
    let source_stopped = !shared_memory::source_window_alive(frame.window)
        || (frame.shutdown > 0 && frame.shutdown >= frame.startup);
    if source_stopped {
        let changed = state.had_source || state.previous.take().is_some();
        state.had_source = false;
        return Ok(if changed {
            WeatherUpdate::NoSession
        } else {
            WeatherUpdate::Unchanged
        });
    }
    let changed = !state.had_source || state.previous != Some(frame.snapshot);
    state.had_source = true;
    state.previous = Some(frame.snapshot);
    Ok(if changed {
        WeatherUpdate::Snapshot(frame.snapshot)
    } else {
        WeatherUpdate::Unchanged
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VehicleKey {
    pub id: i32,
    pub vehicle_filename: String,
    pub vehicle_name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VehicleClass {
    Hypercar,
    Lmp2Elms,
    Lmp2,
    Lmp3,
    Gte,
    Lmgt3,
    PaceCar,
    #[default]
    Unknown,
}

impl VehicleClass {
    pub const ORDER: [Self; 8] = [
        Self::Hypercar,
        Self::Lmp2,
        Self::Lmp2Elms,
        Self::Lmp3,
        Self::Gte,
        Self::Lmgt3,
        Self::PaceCar,
        Self::Unknown,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Hypercar => "HYP",
            Self::Lmp2Elms => "P2E",
            Self::Lmp2 => "P2",
            Self::Lmp3 => "P3",
            Self::Gte => "GTE",
            Self::Lmgt3 => "GT3",
            Self::PaceCar => "SC",
            Self::Unknown => "—",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Hypercar => "Hypercar",
            Self::Lmp2Elms => "LMP2 ELMS",
            Self::Lmp2 => "LMP2",
            Self::Lmp3 => "LMP3",
            Self::Gte => "GTE",
            Self::Lmgt3 => "LMGT3",
            Self::PaceCar => "Pace Car",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VehicleStatus {
    #[default]
    None,
    PitRequested,
    PitIn,
    Pit,
    PitOut,
    Garage,
    Finished,
    Dnf,
    Disqualified,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TireCompound {
    pub front: String,
    pub rear: String,
}

impl TireCompound {
    pub fn label(&self) -> String {
        let front = compact_compound(&self.front);
        let rear = compact_compound(&self.rear);
        match (front, rear) {
            (None, None) => "—".into(),
            (Some(v), None) | (None, Some(v)) => v,
            (Some(a), Some(b)) if a == b => a,
            (Some(a), Some(b)) => format!("F:{a} R:{b}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StandingEntry {
    pub key: VehicleKey,
    pub driver_name: String,
    pub vehicle_name: String,
    pub vehicle_model: String,
    pub class: VehicleClass,
    pub position: u16,
    pub class_position: u16,
    pub laps: u16,
    pub lap_distance: f64,
    pub best_lap_ms: Option<u64>,
    pub last_lap_ms: Option<u64>,
    pub gap_to_leader_ms: Option<u64>,
    pub laps_behind_leader: u16,
    pub gap_to_previous_ms: Option<u64>,
    pub laps_behind_previous: u16,
    pub tires: TireCompound,
    pub status: VehicleStatus,
    pub penalties: u16,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StandingsSnapshot {
    pub session_generation: u64,
    pub session: i32,
    pub track_name: String,
    pub focused_vehicle: Option<VehicleKey>,
    pub entries: Vec<StandingEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StandingsUpdate {
    Snapshot(StandingsSnapshot),
    NoSession,
    Unchanged,
}

#[derive(Default)]
struct StandingsState {
    startup: Option<u32>,
    scoring: Option<u32>,
    telemetry: Option<u32>,
    elapsed: Option<u64>,
    session_start: Option<u32>,
    track: String,
    session: i32,
    generation: u64,
    had_session: bool,
}

pub struct LmuStandings {
    mapping: SharedMemory,
    lock: SharedMemoryLock,
    state: StandingsState,
}

impl LmuStandings {
    pub fn connect() -> Result<Self> {
        Ok(Self {
            mapping: SharedMemory::open(MAPPING_NAME, MAPPING_SIZE)?,
            lock: SharedMemoryLock::open()?,
            state: StandingsState::default(),
        })
    }
    pub fn read(&mut self) -> Result<StandingsUpdate> {
        let Some(_guard) = self.lock.try_lock() else {
            let window: u64 = self.mapping.read(APPLICATION_WINDOW)?;
            if !shared_memory::source_window_alive(window) {
                self.state.had_session = false;
                return Err(Error::SharedMemoryUnavailable {
                    name: MAPPING_NAME.into(),
                    source: "source stopped while the shared lock was busy".into(),
                });
            }
            return Ok(StandingsUpdate::Unchanged);
        };
        let frame = read_standings_frame(&self.mapping)?;
        drop(_guard);
        decode_standings(frame, &mut self.state)
    }
}

struct RawEntry {
    id: i32,
    driver: [u8; 32],
    vehicle: [u8; 64],
    filename: [u8; 32],
    class: [u8; 32],
    laps: i16,
    finish: u8,
    distance: f64,
    best: f64,
    last: f64,
    penalties: i16,
    player: bool,
    in_pits: bool,
    place: u8,
    behind_next: f64,
    laps_next: i32,
    behind_leader: f64,
    laps_leader: i32,
    pit: u8,
    garage: bool,
}

struct RawTelemetry {
    id: i32,
    vehicle: [u8; 64],
    front: [u8; 18],
    rear: [u8; 18],
    model: [u8; 30],
    class: u8,
}

struct RawStandingsFrame {
    startup: u32,
    shutdown: u32,
    session_start: u32,
    scoring: u32,
    telemetry_event: u32,
    window: u64,
    elapsed: f64,
    session: i32,
    track: [u8; 64],
    player_name: [u8; 32],
    player_telemetry_id: Option<i32>,
    entries: Vec<RawEntry>,
    telemetry: Vec<RawTelemetry>,
}

fn read_standings_frame<M: Memory + ?Sized>(memory: &M) -> Result<RawStandingsFrame> {
    let startup = memory.read(STARTUP_EVENT)?;
    let shutdown = memory.read(SHUTDOWN_EVENT)?;
    let session_start = memory.read(START_SESSION_EVENT)?;
    let scoring = memory.read(SCORING_EVENT)?;
    let telemetry_event = memory.read(TELEMETRY_EVENT)?;
    let window = memory.read(APPLICATION_WINDOW)?;
    let elapsed = memory.read(SCORING_CURRENT_ET)?;
    let session = memory.read(SCORING_SESSION)?;
    let track = memory.read(SCORING_TRACK_NAME)?;
    let player_name = memory.read(SCORING_PLAYER_NAME)?;
    let count = memory
        .read::<i32>(SCORING_NUM_VEHICLES)?
        .clamp(0, MAX_VEHICLES as i32) as usize;
    let telemetry_count =
        usize::from(memory.read::<u8>(ACTIVE_TELEMETRY_VEHICLES)?).min(MAX_VEHICLES);
    let player_index = usize::from(memory.read::<u8>(PLAYER_TELEMETRY_INDEX)?);
    let has_player_telemetry = memory.read::<u8>(PLAYER_HAS_TELEMETRY)? != 0;
    let entries = (0..count)
        .map(|i| read_raw_entry(memory, VEHICLE_SCORING + i * VEHICLE_SCORING_SIZE))
        .collect::<Result<Vec<_>>>()?;
    let telemetry = (0..telemetry_count)
        .map(|i| read_raw_telemetry(memory, VEHICLE_TELEMETRY + i * VEHICLE_TELEMETRY_SIZE))
        .collect::<Result<Vec<_>>>()?;
    let player_telemetry_id = has_player_telemetry
        .then(|| telemetry.get(player_index).map(|entry| entry.id))
        .flatten();
    Ok(RawStandingsFrame {
        startup,
        shutdown,
        session_start,
        scoring,
        telemetry_event,
        window,
        elapsed,
        session,
        track,
        player_name,
        player_telemetry_id,
        entries,
        telemetry,
    })
}

fn read_raw_entry<M: Memory + ?Sized>(m: &M, base: usize) -> Result<RawEntry> {
    Ok(RawEntry {
        id: m.read(base)?,
        driver: m.read(base + 4)?,
        vehicle: m.read(base + 36)?,
        laps: m.read::<i16>(base + 100)?,
        finish: m.read(base + 103)?,
        distance: m.read(base + 104)?,
        best: m.read(base + 144)?,
        last: m.read(base + 168)?,
        penalties: m.read::<i16>(base + 194)?,
        player: m.read::<u8>(base + 196)? != 0,
        in_pits: m.read::<u8>(base + 198)? != 0,
        place: m.read(base + 199)?,
        class: m.read(base + 200)?,
        behind_next: m.read(base + 232)?,
        laps_next: m.read(base + 240)?,
        behind_leader: m.read(base + 244)?,
        laps_leader: m.read(base + 252)?,
        pit: m.read(base + 457)?,
        garage: m.read::<u8>(base + 507)? != 0,
        filename: m.read(base + 544)?,
    })
}

fn read_raw_telemetry<M: Memory + ?Sized>(m: &M, base: usize) -> Result<RawTelemetry> {
    Ok(RawTelemetry {
        id: m.read(base)?,
        vehicle: m.read(base + 32)?,
        front: m.read(base + 620)?,
        rear: m.read(base + 638)?,
        model: m.read(base + 796)?,
        class: m.read(base + 826)?,
    })
}

fn decode_standings(
    frame: RawStandingsFrame,
    state: &mut StandingsState,
) -> Result<StandingsUpdate> {
    if !frame.elapsed.is_finite() {
        return Ok(StandingsUpdate::Unchanged);
    }
    let elapsed = frame.elapsed.to_bits();
    let source_stopped = !shared_memory::source_window_alive(frame.window)
        || (frame.shutdown > 0 && frame.shutdown >= frame.startup);
    if source_stopped || frame.entries.is_empty() {
        let changed = state.had_session
            || state.startup != Some(frame.startup)
            || state.scoring != Some(frame.scoring)
            || state.elapsed != Some(elapsed);
        state.startup = Some(frame.startup);
        state.scoring = Some(frame.scoring);
        state.telemetry = Some(frame.telemetry_event);
        state.elapsed = Some(elapsed);
        state.session_start = Some(frame.session_start);
        state.had_session = false;
        return Ok(if changed {
            StandingsUpdate::NoSession
        } else {
            StandingsUpdate::Unchanged
        });
    }
    if state.startup == Some(frame.startup)
        && state.scoring == Some(frame.scoring)
        && state.telemetry == Some(frame.telemetry_event)
        && state.elapsed == Some(elapsed)
        && state.session_start == Some(frame.session_start)
    {
        return Ok(StandingsUpdate::Unchanged);
    }
    let track = decode(&frame.track);
    let restart = !state.had_session
        || state.startup != Some(frame.startup)
        || state.session_start != Some(frame.session_start)
        || state.track != track
        || state.session != frame.session
        || (frame.session_start == 0
            && state
                .elapsed
                .map(f64::from_bits)
                .is_some_and(|old| frame.elapsed + 0.001 < old));
    if restart {
        state.generation = state.generation.wrapping_add(1).max(1);
    }
    state.startup = Some(frame.startup);
    state.scoring = Some(frame.scoring);
    state.telemetry = Some(frame.telemetry_event);
    state.elapsed = Some(elapsed);
    state.session_start = Some(frame.session_start);
    state.track.clone_from(&track);
    state.session = frame.session;
    state.had_session = true;
    let telemetry = frame
        .telemetry
        .into_iter()
        .map(|entry| (entry.id, entry))
        .collect::<HashMap<_, _>>();
    let player_name = decode(&frame.player_name);
    let mut official_focus = None;
    let mut fallback_focus = None;
    let mut entries = frame
        .entries
        .into_iter()
        .map(|raw| {
            let vehicle = decode(&raw.vehicle);
            let filename = decode(&raw.filename);
            let key = VehicleKey {
                id: raw.id,
                vehicle_filename: filename,
                vehicle_name: vehicle.clone(),
            };
            let info = telemetry
                .get(&raw.id)
                .filter(|item| vehicle.eq_ignore_ascii_case(&decode(&item.vehicle)));
            if raw.player && official_focus.is_none() {
                official_focus = Some(key.clone());
            }
            if frame.player_telemetry_id == Some(raw.id)
                && info.is_some()
                && fallback_focus.is_none()
            {
                fallback_focus = Some(key.clone());
            }
            let class = info
                .map(|item| class_from_code(item.class))
                .filter(|class| *class != VehicleClass::Unknown)
                .unwrap_or_else(|| class_from_name(&decode(&raw.class)));
            StandingEntry {
                key,
                driver_name: decode(&raw.driver),
                vehicle_name: vehicle,
                vehicle_model: info.map(|item| decode(&item.model)).unwrap_or_default(),
                class,
                position: u16::from(raw.place),
                class_position: 0,
                laps: raw.laps.max(0) as u16,
                lap_distance: raw.distance,
                best_lap_ms: milliseconds(raw.best),
                last_lap_ms: milliseconds(raw.last),
                gap_to_leader_ms: milliseconds(raw.behind_leader),
                laps_behind_leader: lap_gap(raw.laps_leader),
                gap_to_previous_ms: milliseconds(raw.behind_next),
                laps_behind_previous: lap_gap(raw.laps_next),
                tires: TireCompound {
                    front: info.map(|item| decode(&item.front)).unwrap_or_default(),
                    rear: info.map(|item| decode(&item.rear)).unwrap_or_default(),
                },
                status: status(raw.finish, raw.pit, raw.in_pits, raw.garage),
                penalties: raw.penalties.max(0) as u16,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| sort_entry(a, b, frame.session));
    assign_positions_and_gaps(&mut entries, frame.session);
    let focused_vehicle = official_focus
        .or(fallback_focus)
        .or_else(|| unique_driver_focus(&entries, &player_name));
    Ok(StandingsUpdate::Snapshot(StandingsSnapshot {
        session_generation: state.generation,
        session: frame.session,
        track_name: track,
        focused_vehicle,
        entries,
    }))
}

fn sort_entry(a: &StandingEntry, b: &StandingEntry, session: i32) -> Ordering {
    match (a.position > 0, b.position > 0) {
        (true, true) => return a.position.cmp(&b.position),
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    if (5..=8).contains(&session) {
        a.best_lap_ms
            .cmp(&b.best_lap_ms)
            .then_with(|| a.key.id.cmp(&b.key.id))
    } else {
        b.laps
            .cmp(&a.laps)
            .then_with(|| {
                b.lap_distance
                    .partial_cmp(&a.lap_distance)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| a.key.id.cmp(&b.key.id))
    }
}

fn assign_positions_and_gaps(entries: &mut [StandingEntry], session: i32) {
    let mut class_positions = HashMap::<VehicleClass, u16>::new();
    let mut race_reference = HashMap::<VehicleClass, (u16, u64, u16, u64)>::new();
    let mut qualifying_leader = HashMap::<VehicleClass, u64>::new();
    if (5..=8).contains(&session) {
        for entry in &*entries {
            if let Some(best) = entry.best_lap_ms {
                qualifying_leader
                    .entry(entry.class)
                    .and_modify(|leader| *leader = (*leader).min(best))
                    .or_insert(best);
            }
        }
    }
    let mut qualifying_previous = HashMap::<VehicleClass, u64>::new();
    for entry in entries {
        let position = class_positions.entry(entry.class).or_default();
        *position = position.saturating_add(1);
        entry.class_position = *position;
        if (5..=8).contains(&session) {
            let previous = qualifying_previous.get(&entry.class).copied();
            let leader = qualifying_leader.get(&entry.class).copied();
            entry.gap_to_leader_ms = entry
                .best_lap_ms
                .zip(leader)
                .map(|(lap, best)| lap.saturating_sub(best));
            entry.gap_to_previous_ms = entry
                .best_lap_ms
                .zip(previous)
                .map(|(lap, best)| lap.saturating_sub(best));
            entry.laps_behind_leader = 0;
            entry.laps_behind_previous = 0;
            if let Some(best) = entry.best_lap_ms {
                qualifying_previous.insert(entry.class, best);
            }
        } else {
            let total_laps = entry.laps_behind_leader;
            let total_gap = entry.gap_to_leader_ms.unwrap_or(0);
            let reference = race_reference
                .entry(entry.class)
                .or_insert((total_laps, total_gap, total_laps, total_gap));
            let leader_laps = total_laps.saturating_sub(reference.0);
            let previous_laps = total_laps.saturating_sub(reference.2);
            entry.gap_to_leader_ms =
                (leader_laps == 0).then(|| total_gap.saturating_sub(reference.1));
            entry.gap_to_previous_ms =
                (previous_laps == 0).then(|| total_gap.saturating_sub(reference.3));
            entry.laps_behind_leader = leader_laps;
            entry.laps_behind_previous = previous_laps;
            reference.2 = total_laps;
            reference.3 = total_gap;
        }
    }
}

fn unique_driver_focus(entries: &[StandingEntry], player: &str) -> Option<VehicleKey> {
    if player.is_empty() {
        return None;
    }
    let mut matching = entries
        .iter()
        .filter(|entry| entry.driver_name.eq_ignore_ascii_case(player));
    let first = matching.next()?;
    matching.next().is_none().then(|| first.key.clone())
}

pub fn select_visible_entries(
    entries: &[StandingEntry],
    focused: Option<&VehicleKey>,
    limit: usize,
) -> Vec<StandingEntry> {
    let limit = limit.max(1).min(entries.len());
    if entries.len() <= limit {
        return entries.to_vec();
    }
    let Some(index) = focused.and_then(|key| entries.iter().position(|entry| &entry.key == key))
    else {
        return entries[..limit].to_vec();
    };
    if index < 3 || limit <= 3 {
        return entries[..limit].to_vec();
    }
    let local = limit - 3;
    let start = index
        .saturating_sub(local / 2)
        .min(entries.len() - local)
        .max(3);
    entries[..3]
        .iter()
        .chain(entries[start..start + local].iter())
        .cloned()
        .collect()
}

pub fn select_class_blocks(
    entries: &[StandingEntry],
    focused: Option<&VehicleKey>,
    limit: usize,
    classes: &[VehicleClass],
) -> Vec<(VehicleClass, Vec<StandingEntry>)> {
    VehicleClass::ORDER
        .into_iter()
        .filter(|class| classes.contains(class))
        .filter_map(|class| {
            let members = entries
                .iter()
                .filter(|entry| entry.class == class)
                .cloned()
                .collect::<Vec<_>>();
            (!members.is_empty()).then(|| (class, select_visible_entries(&members, focused, limit)))
        })
        .collect()
}

fn decode<const N: usize>(value: &[u8; N]) -> String {
    let end = value.iter().position(|byte| *byte == 0).unwrap_or(N);
    String::from_utf8_lossy(&value[..end]).trim().to_owned()
}
fn milliseconds(value: f64) -> Option<u64> {
    (value.is_finite() && value > 0.)
        .then(|| (value * 1000.).round())
        .filter(|value| *value <= u64::MAX as f64)
        .map(|value| value as u64)
}
fn lap_gap(value: i32) -> u16 {
    value.clamp(0, u16::MAX as i32) as u16
}
fn class_from_code(value: u8) -> VehicleClass {
    match value {
        0 => VehicleClass::Hypercar,
        2 => VehicleClass::Lmp2Elms,
        3 => VehicleClass::Lmp2,
        4 => VehicleClass::Lmp3,
        5 => VehicleClass::Gte,
        6 => VehicleClass::Lmgt3,
        8 => VehicleClass::PaceCar,
        _ => VehicleClass::Unknown,
    }
}
fn class_from_name(value: &str) -> VehicleClass {
    let value = value.to_ascii_lowercase();
    if value.contains("hypercar") || value.contains("lmh") || value.contains("lmdh") {
        VehicleClass::Hypercar
    } else if value.contains("lmp2") && value.contains("elms") {
        VehicleClass::Lmp2Elms
    } else if value.contains("lmp2") {
        VehicleClass::Lmp2
    } else if value.contains("lmp3") {
        VehicleClass::Lmp3
    } else if value.contains("lmgt3") || value == "gt3" || value.contains(" gt3") {
        VehicleClass::Lmgt3
    } else if value.contains("gte") {
        VehicleClass::Gte
    } else if value.contains("pace") || value.contains("safety") {
        VehicleClass::PaceCar
    } else {
        VehicleClass::Unknown
    }
}
fn compact_compound(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let value_lower = value.to_ascii_lowercase();
    let label = if value_lower.contains("soft") {
        "S"
    } else if value_lower.contains("medium") {
        "M"
    } else if value_lower.contains("hard") {
        "H"
    } else if value_lower.contains("wet") {
        "W"
    } else {
        return Some(value.chars().take(6).collect());
    };
    Some(label.into())
}
fn status(finish: u8, pit: u8, in_pits: bool, garage: bool) -> VehicleStatus {
    match finish {
        3 => VehicleStatus::Disqualified,
        2 => VehicleStatus::Dnf,
        1 => VehicleStatus::Finished,
        _ if garage => VehicleStatus::Garage,
        _ => match pit {
            1 => VehicleStatus::PitRequested,
            2 => VehicleStatus::PitIn,
            3 => VehicleStatus::Pit,
            4 => VehicleStatus::PitOut,
            _ if in_pits => VehicleStatus::Pit,
            _ => VehicleStatus::None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_f64(memory: &mut [u8], offset: usize, value: f64) {
        memory[offset..offset + size_of::<f64>()].copy_from_slice(&value.to_ne_bytes());
    }

    #[test]
    fn weather_snapshot_decodes_scoring_fields() {
        let mut memory = vec![0; VEHICLE_SCORING];
        write_f64(&mut memory, SCORING_CLOUD_DARKNESS, 0.25);
        write_f64(&mut memory, SCORING_RAIN_INTENSITY, 0.5);
        write_f64(&mut memory, SCORING_AMBIENT_TEMPERATURE, 21.5);
        write_f64(&mut memory, SCORING_TRACK_TEMPERATURE, 29.25);
        write_f64(&mut memory, SCORING_WIND, -2.0);
        write_f64(&mut memory, SCORING_WIND + size_of::<f64>(), 0.5);
        write_f64(&mut memory, SCORING_WIND + size_of::<f64>() * 2, 4.0);
        write_f64(&mut memory, SCORING_MIN_PATH_WETNESS, 0.1);
        write_f64(&mut memory, SCORING_MAX_PATH_WETNESS, 0.8);
        write_f64(&mut memory, SCORING_AVERAGE_PATH_WETNESS, 0.4);

        let frame = read_weather_frame(memory.as_slice()).unwrap();

        assert_eq!(
            frame.snapshot,
            WeatherSnapshot {
                cloud_darkness: 0.25,
                rain_intensity: 0.5,
                ambient_temperature_celsius: 21.5,
                track_temperature_celsius: 29.25,
                wind: Wind {
                    x: -2.0,
                    y: 0.5,
                    z: 4.0,
                },
                minimum_path_wetness: 0.1,
                maximum_path_wetness: 0.8,
                average_path_wetness: 0.4,
            }
        );
    }

    #[test]
    fn class_blocks_select_the_leader_and_focused_neighborhood() {
        let entries = (1..=12)
            .map(|position| StandingEntry {
                key: VehicleKey {
                    id: position,
                    vehicle_filename: format!("{position}.veh"),
                    vehicle_name: format!("Car {position}"),
                },
                driver_name: String::new(),
                vehicle_name: String::new(),
                vehicle_model: String::new(),
                class: VehicleClass::Hypercar,
                position: position as u16,
                class_position: position as u16,
                laps: 0,
                lap_distance: 0.,
                best_lap_ms: None,
                last_lap_ms: None,
                gap_to_leader_ms: None,
                laps_behind_leader: 0,
                gap_to_previous_ms: None,
                laps_behind_previous: 0,
                tires: TireCompound::default(),
                status: VehicleStatus::None,
                penalties: 0,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            select_visible_entries(&entries, Some(&entries[8].key), 7)
                .iter()
                .map(|entry| entry.position)
                .collect::<Vec<_>>(),
            [1, 2, 3, 7, 8, 9, 10]
        );
    }

    #[test]
    fn tire_labels_are_compact() {
        assert_eq!(
            TireCompound {
                front: "Medium".into(),
                rear: "Hard".into()
            }
            .label(),
            "F:M R:H"
        );
        assert_eq!(TireCompound::default().label(), "—");
    }
}
