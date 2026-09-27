use crate::project::config;
use maolan_host_adapter::{PluginCatalogEntry, PluginFormat};
use neo_media_ffmpeg::default_hw_device_type_priority;
use shipyard::Unique;

pub mod preset_store;
pub use preset_store::{PresetData, PresetStore};
pub mod source_size_cache;

#[derive(Clone, Debug, Unique)]
pub struct ProjectResource {
    pub name: String,
    pub dir: Option<std::path::PathBuf>,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub audio_sample_rate: u32,
    pub audio_channels: u32,
}

impl ProjectResource {
    pub const DEFAULT_WIDTH: u32 = config::PROJECT_DEFAULT_WIDTH;
    pub const DEFAULT_HEIGHT: u32 = config::PROJECT_DEFAULT_HEIGHT;

    pub fn new() -> Self {
        Self {
            name: String::new(),
            dir: None,
            fps: config::PROJECT_DEFAULT_FPS,
            width: Self::DEFAULT_WIDTH,
            height: Self::DEFAULT_HEIGHT,
            audio_sample_rate: config::PROJECT_DEFAULT_AUDIO_SAMPLE_RATE,
            audio_channels: config::PROJECT_DEFAULT_AUDIO_CHANNELS,
        }
    }
}

pub const DEFAULT_LAYER_COUNT: usize = config::DEFAULT_LAYER_COUNT;

fn default_total_frames() -> i32 {
    config::DEFAULT_TOTAL_FRAMES
}

fn default_layer_states() -> Vec<(bool, bool)> {
    vec![(true, false); DEFAULT_LAYER_COUNT]
}

#[derive(Unique)]
pub struct TimelineResource {
    pub current_frame: i32,
    pub total_frames: i32,
    pub next_id: usize,
    pub zoom_scale: f32,
    pub layer_count: i32,
}

impl TimelineResource {
    pub fn new() -> Self {
        Self {
            current_frame: 0,
            total_frames: config::DEFAULT_TOTAL_FRAMES,
            next_id: 1,
            zoom_scale: 1.0,
            layer_count: usize_to_i32(DEFAULT_LAYER_COUNT),
        }
    }
}

#[derive(Unique)]
pub struct LayerStates(pub Vec<(bool, bool)>);

impl LayerStates {
    pub fn new(count: usize) -> Self {
        Self(vec![(true, false); count])
    }

    pub fn set_visible(&mut self, layer: usize, v: bool) {
        if let Some(s) = self.0.get_mut(layer) {
            s.0 = v;
        }
    }

    pub fn set_locked(&mut self, layer: usize, v: bool) {
        if let Some(s) = self.0.get_mut(layer) {
            s.1 = v;
        }
    }
}

pub const GRID_MODE_AUTO: i32 = 0;

#[derive(Clone, Debug)]
pub struct SceneMeta {
    pub id: i32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub total_frames: i32,
    pub layer_states: Vec<(bool, bool)>,

    pub grid_mode: i32,
    pub grid_bpm: f32,
    pub grid_offset: f32,
    pub grid_interval: i32,
    pub grid_subdivision: i32,
    pub enable_snap: bool,
    pub magnetic_snap_range: i32,
}

impl SceneMeta {
    pub fn new(id: i32, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            width: ProjectResource::DEFAULT_WIDTH,
            height: ProjectResource::DEFAULT_HEIGHT,
            fps: config::PROJECT_DEFAULT_FPS,
            total_frames: default_total_frames(),
            layer_states: default_layer_states(),
            grid_mode: GRID_MODE_AUTO,
            grid_bpm: config::SCENE_DEFAULT_GRID_BPM,
            grid_offset: config::SCENE_DEFAULT_GRID_OFFSET,
            grid_interval: config::SCENE_DEFAULT_GRID_INTERVAL,
            grid_subdivision: config::SCENE_DEFAULT_GRID_SUBDIVISION,
            enable_snap: config::SCENE_DEFAULT_ENABLE_SNAP,
            magnetic_snap_range: config::SCENE_DEFAULT_MAGNETIC_SNAP_RANGE,
        }
    }

    pub fn new_with_defaults(
        id: i32,
        name: impl Into<String>,
        default_snap: bool,
        magnetic_snap_range: i32,
    ) -> Self {
        let mut meta = Self::new(id, name);
        meta.enable_snap = default_snap;
        meta.magnetic_snap_range = magnetic_snap_range;
        meta
    }

    pub fn effective_grid_interval(&self) -> i32 {
        let fps = u32_to_f32(self.fps.max(1));
        let interval = match self.grid_mode {
            1 => {
                let beats_per_minute = self.grid_bpm.max(1.0);
                let beat_frames = (fps * 60.0 / beats_per_minute).max(1.0);
                let subdivision = i32_to_f32(self.grid_subdivision.max(1));
                f32_to_i32((beat_frames / subdivision).round().max(1.0))
            }
            2 => self.grid_interval.max(1),
            _ => f32_to_i32(fps.round().max(1.0)),
        };
        interval.max(1)
    }

    pub fn snap_frame(&self, frame: i32) -> i32 {
        if !self.enable_snap {
            return frame;
        }
        let interval = self.effective_grid_interval();
        if interval <= 0 {
            return frame;
        }
        let offset_frames = f32_to_i32(self.grid_offset * u32_to_f32(self.fps.max(1)));
        let relative = frame.saturating_sub(offset_frames);
        let nearest = f32_to_i32((i32_to_f32(relative) / i32_to_f32(interval)).round())
            .saturating_mul(interval)
            .saturating_add(offset_frames);
        if nearest.saturating_sub(frame).abs() <= self.magnetic_snap_range {
            nearest
        } else {
            frame
        }
    }
}

fn usize_to_i32(value: usize) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn u32_to_f32(value: u32) -> f32 {
    value.to_string().parse().unwrap_or(f32::MAX)
}

fn i32_to_f32(value: i32) -> f32 {
    value.to_string().parse().unwrap_or_else(|_| {
        if value.is_negative() {
            f32::MIN
        } else {
            f32::MAX
        }
    })
}

fn f32_to_i32(value: f32) -> i32 {
    if !value.is_finite() {
        return if value.is_sign_negative() {
            i32::MIN
        } else {
            i32::MAX
        };
    }
    value.round().to_string().parse().unwrap_or_else(|_| {
        if value.is_sign_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

#[derive(Unique)]
pub struct SceneResource {
    pub scenes: Vec<SceneMeta>,
    pub active_scene: i32,
    pub next_scene_id: i32,
}

impl SceneResource {
    pub fn new() -> Self {
        Self {
            scenes: vec![SceneMeta::new(0, "Scene 1")],
            active_scene: 0,
            next_scene_id: 1,
        }
    }

    pub fn find(&self, id: i32) -> Option<&SceneMeta> {
        self.scenes.iter().find(|s| s.id == id)
    }

    pub fn find_mut(&mut self, id: i32) -> Option<&mut SceneMeta> {
        self.scenes.iter_mut().find(|s| s.id == id)
    }
}

#[derive(Clone, Debug, Unique)]
pub struct SystemSettingsResource {
    pub autosave_enabled: bool,
    pub autosave_interval_sec: i32,
    pub theme_dark: bool,
    pub theme_id: String,
    pub easing_engine_id: String,
    pub ui_scale_percent: i32,
    pub worker_threads: i32,
    pub audio_max_block_size: i32,
    pub decode_backend: i32,
    pub default_snap: bool,
    pub magnetic_snap_range: i32,
    pub check_update_on_startup: bool,
    pub crash_reporting_enabled: bool,
    pub max_group_chain_depth: i32,
    pub hw_decode_extra_frames: i32,
    pub hw_device_type_priority: Vec<String>,
    pub codec_decode_overrides: Vec<CodecDecodeOverride>,
}

#[derive(Clone, Debug, Default)]
pub struct CodecDecodeOverride {
    pub codec_kind: String,
    pub force_sw_decode: bool,
    pub custom_priority_enabled: bool,
    pub hw_device_type_priority: Vec<String>,
}

fn codec_decode_override_to_schema(
    value: &CodecDecodeOverride,
) -> neoutl_schema::CodecDecodeOverride {
    neoutl_schema::CodecDecodeOverride {
        codec_kind: value.codec_kind.clone(),
        force_sw_decode: value.force_sw_decode,
        custom_priority_enabled: value.custom_priority_enabled,
        hw_device_type_priority: value.hw_device_type_priority.clone(),
    }
}

fn codec_decode_override_from_schema(
    value: &neoutl_schema::CodecDecodeOverride,
) -> CodecDecodeOverride {
    CodecDecodeOverride {
        codec_kind: value.codec_kind.clone(),
        force_sw_decode: value.force_sw_decode,
        custom_priority_enabled: value.custom_priority_enabled,
        hw_device_type_priority: value.hw_device_type_priority.clone(),
    }
}

impl From<&SystemSettingsResource> for neoutl_schema::SystemSettings {
    fn from(value: &SystemSettingsResource) -> Self {
        Self {
            autosave_enabled: value.autosave_enabled,
            autosave_interval_sec: value.autosave_interval_sec,
            theme_dark: value.theme_dark,
            theme_id: value.theme_id.clone(),
            easing_engine_id: value.easing_engine_id.clone(),
            ui_scale_percent: value.ui_scale_percent,
            worker_threads: value.worker_threads,
            audio_max_block_size: value.audio_max_block_size,
            decode_backend: value.decode_backend,
            default_snap: value.default_snap,
            magnetic_snap_range: value.magnetic_snap_range,
            check_update_on_startup: value.check_update_on_startup,
            crash_reporting_enabled: value.crash_reporting_enabled,
            max_group_chain_depth: value.max_group_chain_depth,
            hw_decode_extra_frames: value.hw_decode_extra_frames,
            hw_device_type_priority: value.hw_device_type_priority.clone(),
            codec_decode_overrides: value
                .codec_decode_overrides
                .iter()
                .map(codec_decode_override_to_schema)
                .collect(),
        }
    }
}

impl TryFrom<&neoutl_schema::SystemSettings> for SystemSettingsResource {
    type Error = String;

    fn try_from(value: &neoutl_schema::SystemSettings) -> Result<Self, Self::Error> {
        Ok(Self {
            autosave_enabled: value.autosave_enabled,
            autosave_interval_sec: value.autosave_interval_sec,
            theme_dark: value.theme_dark,
            theme_id: value.theme_id.clone(),
            easing_engine_id: value.easing_engine_id.clone(),
            ui_scale_percent: value.ui_scale_percent,
            worker_threads: value.worker_threads,
            audio_max_block_size: value.audio_max_block_size,
            decode_backend: value.decode_backend,
            default_snap: value.default_snap,
            magnetic_snap_range: value.magnetic_snap_range,
            check_update_on_startup: value.check_update_on_startup,
            crash_reporting_enabled: value.crash_reporting_enabled,
            max_group_chain_depth: value.max_group_chain_depth,
            hw_decode_extra_frames: if value.hw_decode_extra_frames
                >= config::HW_DECODE_EXTRA_FRAMES_MIN
            {
                value.hw_decode_extra_frames
            } else {
                config::SYSTEM_DEFAULT_HW_DECODE_EXTRA_FRAMES
            },
            hw_device_type_priority: if value.hw_device_type_priority.is_empty() {
                default_hw_device_type_priority()
            } else {
                value.hw_device_type_priority.clone()
            },
            codec_decode_overrides: value
                .codec_decode_overrides
                .iter()
                .map(codec_decode_override_from_schema)
                .collect(),
        })
    }
}

impl From<&SceneMeta> for neoutl_schema::SceneMeta {
    fn from(value: &SceneMeta) -> Self {
        Self {
            id: value.id,
            name: value.name.clone(),
            width: value.width,
            height: value.height,
            fps: value.fps,
            grid_mode: value.grid_mode,
            grid_bpm: value.grid_bpm,
            grid_offset: value.grid_offset,
            grid_interval: value.grid_interval,
            grid_subdivision: value.grid_subdivision,
            enable_snap: value.enable_snap,
            magnetic_snap_range: value.magnetic_snap_range,
        }
    }
}

impl TryFrom<&neoutl_schema::SceneMeta> for SceneMeta {
    type Error = String;

    fn try_from(value: &neoutl_schema::SceneMeta) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            name: value.name.clone(),
            width: value.width,
            height: value.height,
            fps: value.fps,
            total_frames: default_total_frames(),
            layer_states: default_layer_states(),
            grid_mode: value.grid_mode,
            grid_bpm: value.grid_bpm,
            grid_offset: value.grid_offset,
            grid_interval: value.grid_interval,
            grid_subdivision: value.grid_subdivision,
            enable_snap: value.enable_snap,
            magnetic_snap_range: value.magnetic_snap_range,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_grid_uses_subdivision_frames() {
        let mut meta = SceneMeta::new(0, "Scene 1");
        meta.fps = 30;
        meta.grid_mode = 1;
        meta.grid_bpm = 120.0;
        meta.grid_subdivision = 2;
        meta.enable_snap = true;
        meta.magnetic_snap_range = 10;

        assert_eq!(meta.effective_grid_interval(), 8);
        assert_eq!(meta.snap_frame(7), 8);
        assert_eq!(meta.snap_frame(15), 16);
    }

    #[test]
    fn fixed_frame_grid_uses_configured_interval() {
        let mut meta = SceneMeta::new(0, "Scene 1");
        meta.grid_mode = 2;
        meta.grid_interval = 24;
        meta.enable_snap = true;
        meta.magnetic_snap_range = 10;

        assert_eq!(meta.effective_grid_interval(), 24);
        assert_eq!(meta.snap_frame(23), 24);
        assert_eq!(meta.snap_frame(25), 24);
    }
}

impl Default for SystemSettingsResource {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemSettingsResource {
    pub fn new() -> Self {
        Self {
            autosave_enabled: config::SYSTEM_DEFAULT_AUTOSAVE_ENABLED,
            autosave_interval_sec: config::SYSTEM_DEFAULT_AUTOSAVE_INTERVAL_SEC,
            theme_dark: config::SYSTEM_DEFAULT_THEME_DARK,
            theme_id: config::SYSTEM_DEFAULT_THEME_ID.to_string(),
            easing_engine_id: config::SYSTEM_DEFAULT_EASING_ENGINE_ID.to_string(),
            ui_scale_percent: config::SYSTEM_DEFAULT_UI_SCALE_PERCENT,
            worker_threads: config::SYSTEM_DEFAULT_WORKER_THREADS,
            audio_max_block_size: config::SYSTEM_DEFAULT_AUDIO_MAX_BLOCK_SIZE,
            decode_backend: config::SYSTEM_DEFAULT_DECODE_BACKEND,
            default_snap: config::SYSTEM_DEFAULT_DEFAULT_SNAP,
            magnetic_snap_range: config::SYSTEM_DEFAULT_MAGNETIC_SNAP_RANGE,
            check_update_on_startup: config::SYSTEM_DEFAULT_CHECK_UPDATE_ON_STARTUP,
            crash_reporting_enabled: config::SYSTEM_DEFAULT_CRASH_REPORTING_ENABLED,
            max_group_chain_depth: config::SYSTEM_DEFAULT_MAX_GROUP_CHAIN_DEPTH,
            hw_decode_extra_frames: config::SYSTEM_DEFAULT_HW_DECODE_EXTRA_FRAMES,
            hw_device_type_priority: default_hw_device_type_priority(),
            codec_decode_overrides: Vec::new(),
        }
    }
}

fn plugin_format_to_i32(format: PluginFormat) -> i32 {
    match format {
        PluginFormat::Vst3 => 0,
        PluginFormat::Clap => 1,
        PluginFormat::Lv2 => 2,
        PluginFormat::Vst2 => 3,
        PluginFormat::Au => 4,
        PluginFormat::Sf2 => 5,
        PluginFormat::Sfz => 6,
        PluginFormat::Jsfx => 7,
        PluginFormat::Internal => 8,
    }
}

fn plugin_format_from_i32(value: i32) -> PluginFormat {
    match value {
        0 => PluginFormat::Vst3,
        1 => PluginFormat::Clap,
        2 => PluginFormat::Lv2,
        3 => PluginFormat::Vst2,
        4 => PluginFormat::Au,
        5 => PluginFormat::Sf2,
        6 => PluginFormat::Sfz,
        7 => PluginFormat::Jsfx,
        _ => PluginFormat::Internal,
    }
}

#[derive(Clone, Debug, Unique)]
pub struct AudioPluginSettingsResource {
    pub scan_paths: Vec<String>,
    pub disabled_plugin_ids: Vec<String>,
    pub cached_catalog: Vec<PluginCatalogEntry>,
    pub auto_detect_system: bool,
}

impl Default for AudioPluginSettingsResource {
    fn default() -> Self {
        Self {
            scan_paths: Vec::new(),
            disabled_plugin_ids: Vec::new(),
            cached_catalog: Vec::new(),
            auto_detect_system: true,
        }
    }
}

impl AudioPluginSettingsResource {
    pub fn new() -> Self {
        Self::default()
    }
}

fn plugin_catalog_entry_to_schema(
    value: &PluginCatalogEntry,
) -> neoutl_schema::AudioPluginCatalogEntry {
    neoutl_schema::AudioPluginCatalogEntry {
        format: plugin_format_to_i32(value.format),
        name: value.name.clone(),
        vendor: value.vendor.clone(),
        plugin_id: value.plugin_id.clone(),
        path: value.path.to_string_lossy().to_string(),
    }
}

fn plugin_catalog_entry_from_schema(
    value: &neoutl_schema::AudioPluginCatalogEntry,
) -> PluginCatalogEntry {
    PluginCatalogEntry {
        format: plugin_format_from_i32(value.format),
        name: value.name.clone(),
        vendor: value.vendor.clone(),
        plugin_id: value.plugin_id.clone(),
        path: std::path::PathBuf::from(&value.path),
    }
}

impl From<&AudioPluginSettingsResource> for neoutl_schema::AudioPluginSettings {
    fn from(value: &AudioPluginSettingsResource) -> Self {
        Self {
            scan_paths: value.scan_paths.clone(),
            disabled_plugin_ids: value.disabled_plugin_ids.clone(),
            cached_catalog: value
                .cached_catalog
                .iter()
                .map(plugin_catalog_entry_to_schema)
                .collect(),
            auto_detect_system: value.auto_detect_system,
        }
    }
}

impl TryFrom<&neoutl_schema::AudioPluginSettings> for AudioPluginSettingsResource {
    type Error = String;

    fn try_from(value: &neoutl_schema::AudioPluginSettings) -> Result<Self, Self::Error> {
        Ok(Self {
            scan_paths: value.scan_paths.clone(),
            disabled_plugin_ids: value.disabled_plugin_ids.clone(),
            cached_catalog: value
                .cached_catalog
                .iter()
                .map(plugin_catalog_entry_from_schema)
                .collect(),
            auto_detect_system: value.auto_detect_system,
        })
    }
}
