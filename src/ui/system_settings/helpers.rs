use crate::ecs::resources::SystemSettingsResource;
use std::path::PathBuf;

#[derive(Clone, Default)]
pub(super) enum ScanStatus {
    #[default]
    Idle,
    Scanning,
    Done,
    Error(String),
}

pub(super) fn category_label(index: usize) -> &'static str {
    super::window::CATEGORIES[index].0
}

pub(super) fn hw_backend_display_name(id: &str) -> String {
    match id {
        "cuda" => "CUDA (NVIDIA)".to_owned(),
        "qsv" => "QSV (Intel)".to_owned(),
        "d3d11va" => "D3D11VA (Windows)".to_owned(),
        "d3d12va" => "D3D12VA (Windows)".to_owned(),
        "dxva2" => "DXVA2 (Windows)".to_owned(),
        "videotoolbox" => "VideoToolbox (macOS)".to_owned(),
        "vulkan" => "Vulkan".to_owned(),
        "opencl" => "OpenCL".to_owned(),
        "vdpau" => "VDPAU (Linux)".to_owned(),
        "amf" => "AMF (AMD)".to_owned(),
        "mediacodec" => "MediaCodec (Android)".to_owned(),
        "drm" => "DRM (Linux)".to_owned(),
        "vaapi" => "VAAPI (Linux)".to_owned(),
        other => other.to_owned(),
    }
}

pub(super) fn codec_display_name(codec_kind: &str) -> String {
    match codec_kind {
        "av1" => "AV1".to_owned(),
        "hevc" => "HEVC (H.265)".to_owned(),
        "h264" => "H.264".to_owned(),
        "vp9" => "VP9".to_owned(),
        "vp8" => "VP8".to_owned(),
        "mpeg2" => "MPEG-2".to_owned(),
        "mpeg4" => "MPEG-4".to_owned(),
        "vc1" => "VC-1".to_owned(),
        other => other.to_owned(),
    }
}

pub(super) fn settings_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()
                .map(|d| d.join("settings").join("system-settings.npb"))
        })
        .unwrap_or_else(|| PathBuf::from("settings/system-settings.npb"))
}

pub(super) fn save_to_disk(s: &SystemSettingsResource) -> std::io::Result<()> {
    let path = settings_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let encoded = crate::project::schema::encode_schema(s);
    std::fs::write(path, encoded)
}

pub(crate) fn load_from_disk() -> Option<SystemSettingsResource> {
    let bytes = std::fs::read(settings_path()).ok()?;
    crate::project::schema::decode_schema::<SystemSettingsResource>(&bytes).ok()
}

pub(super) fn easing_engine_ids_and_names() -> (Vec<String>, Vec<String>) {
    let ids = crate::easings::loader::registry()
        .iter()
        .map(|e| e.id.clone())
        .collect();
    let names = crate::easings::loader::registry()
        .iter()
        .map(|e| e.name.clone())
        .collect();
    (ids, names)
}

pub(super) fn index_of(ids: &[String], id: &str) -> i32 {
    ids.iter().position(|i| i == id).map_or(0, |i| i as i32)
}
