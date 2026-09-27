use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

use ffmpeg_sys_next as sys;

use super::pixfmt::{
    av_pix_fmt_bgr0, av_pix_fmt_none, av_pix_fmt_nv12, av_pix_fmt_p010le, av_pix_fmt_p012le,
    av_pix_fmt_p016le, av_pix_fmt_rgb0, av_pix_fmt_yuv420p, av_pix_fmt_yuv420p10le,
    av_pix_fmt_yuv420p12le, av_pix_fmt_yuvj420p,
};

pub(crate) struct HwPixFmtBox {
    pub(crate) pix_fmt: i32,
}

pub(crate) fn resolve_hw_sw_format(stream_sw_format: i32) -> Option<i32> {
    if stream_sw_format == av_pix_fmt_nv12()
        || stream_sw_format == av_pix_fmt_yuv420p()
        || stream_sw_format == av_pix_fmt_yuvj420p()
        || stream_sw_format == av_pix_fmt_yuv420p10le()
    {
        Some(av_pix_fmt_p010le())
    } else if stream_sw_format == av_pix_fmt_yuv420p12le() {
        Some(av_pix_fmt_p012le())
    } else if stream_sw_format == av_pix_fmt_p010le()
        || stream_sw_format == av_pix_fmt_p012le()
        || stream_sw_format == av_pix_fmt_p016le()
        || stream_sw_format == av_pix_fmt_rgb0()
        || stream_sw_format == av_pix_fmt_bgr0()
    {
        Some(stream_sw_format)
    } else {
        None
    }
}

pub(crate) unsafe extern "C" fn hw_get_format(
    ctx: *mut sys::AVCodecContext,
    pixfmts: *const sys::AVPixelFormat,
) -> sys::AVPixelFormat {
    unsafe {
        let hw_box = if (*ctx).opaque.is_null() {
            None
        } else {
            Some(&*((*ctx).opaque as *const HwPixFmtBox))
        };
        let hw_pix_fmt = hw_box.map(|b| b.pix_fmt).unwrap_or(av_pix_fmt_none());
        let mut p = pixfmts;
        while std::mem::transmute::<sys::AVPixelFormat, i32>(*p) != av_pix_fmt_none() {
            if std::mem::transmute::<sys::AVPixelFormat, i32>(*p) == hw_pix_fmt {
                return *p;
            }
            p = p.add(1);
        }
        *pixfmts
    }
}

pub(crate) fn poisoned_hw_registry() -> &'static Mutex<HashMap<PathBuf, HashSet<i32>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, HashSet<i32>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// codecのhw configを列挙し、hw_device_type_priority_store()に登録された
/// device_type名のうち最も優先順位の高いものを一致させたrank (小さいほど優先) を返す。
/// method対象はAV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTXのみ。
/// このmethodはget_format+hw_device_ctx方式によるhwaccel/software両対応を意味し、
/// hw専用ラッパー実装 (例: h264_cuvid、method=AV_CODEC_HW_CONFIG_METHOD_INTERNAL) は
/// このmethodを広告しないため候補から自然に除外される。
/// 一致するdevice_typeが1つもない場合はNoneを返す。
unsafe fn hw_config_priority_rank(
    codec: *const sys::AVCodec,
    priority: &[String],
) -> Option<usize> {
    unsafe {
        let mut i = 0;
        let mut best_rank: Option<usize> = None;
        loop {
            let config = sys::avcodec_get_hw_config(codec, i);
            if config.is_null() {
                break;
            }
            i += 1;
            let methods = (*config).methods;
            if (methods & sys::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) == 0 {
                continue;
            }
            let name_ptr = sys::av_hwdevice_get_type_name((*config).device_type);
            if name_ptr.is_null() {
                continue;
            }
            let name = CStr::from_ptr(name_ptr).to_string_lossy();
            let Some(rank) = priority.iter().position(|p| p.as_str() == name) else {
                continue;
            };
            best_rank = Some(best_rank.map_or(rank, |b| b.min(rank)));
        }
        best_rank
    }
}

/// codec_idに対応する全デコーダ実装 (例: AV1のnative実装とlibdav1d) を走査し、
/// hw_device_type_priority_store()の優先順位に最も早く一致するhw config
/// (AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX) を持つ実装を選ぶ。
/// 一致する実装が存在しない場合はavcodec_find_decoder(codec_id)の結果にフォールバックする。
/// この関数の戻り値がNULLになるのは、avcodec_find_decoderがNULLを返す場合、
/// すなわち対応デコーダが存在しない場合のみである。
pub(crate) unsafe fn find_decoder_for_codec_id(codec_id: sys::AVCodecID) -> *const sys::AVCodec {
    unsafe {
        let priority = hw_device_type_priority_store().lock().unwrap();
        let mut best: Option<(*const sys::AVCodec, usize)> = None;
        let mut iter_state: *mut c_void = ptr::null_mut();
        loop {
            let codec = sys::av_codec_iterate(&mut iter_state);
            if codec.is_null() {
                break;
            }
            if (*codec).id != codec_id || sys::av_codec_is_decoder(codec) == 0 {
                continue;
            }
            let Some(rank) = hw_config_priority_rank(codec, &priority) else {
                continue;
            };
            if best.is_none_or(|(_, best_rank)| rank < best_rank) {
                best = Some((codec, rank));
            }
        }
        match best {
            Some((codec, _)) => codec,
            None => sys::avcodec_find_decoder(codec_id),
        }
    }
}

pub(crate) fn poisoned_hw_types_for(path: &Path) -> HashSet<i32> {
    poisoned_hw_registry()
        .lock()
        .unwrap()
        .get(path)
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn mark_hw_device_poisoned(path: &Path, device_type_i32: i32) {
    eprintln!(
        "[neoutl-video-decoder][診断][hw_poisoned登録] path={} device_type={device_type_i32}",
        path.display()
    );
    poisoned_hw_registry()
        .lock()
        .unwrap()
        .entry(path.to_path_buf())
        .or_default()
        .insert(device_type_i32);
}

const HW_DEVICE_TYPE_PRIORITY_DEFAULT: &[&str] = &[
    "cuda",
    "qsv",
    "d3d11va",
    "d3d12va",
    "dxva2",
    "videotoolbox",
    "vulkan",
    "opencl",
    "vdpau",
    "amf",
    "mediacodec",
    "drm",
    "vaapi",
];

pub(crate) fn hw_device_type_priority_store() -> &'static Mutex<Vec<String>> {
    static STORE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    STORE.get_or_init(|| {
        Mutex::new(
            HW_DEVICE_TYPE_PRIORITY_DEFAULT
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
    })
}

pub fn default_hw_device_type_priority() -> Vec<String> {
    HW_DEVICE_TYPE_PRIORITY_DEFAULT
        .iter()
        .map(|s| s.to_string())
        .collect()
}

pub fn set_hw_device_type_priority(order: Vec<String>) {
    let valid: Vec<String> = order
        .into_iter()
        .filter(|name| HW_DEVICE_TYPE_PRIORITY_DEFAULT.contains(&name.as_str()))
        .collect();
    *hw_device_type_priority_store().lock().unwrap() = valid;
}

pub(crate) fn available_hw_device_types() -> Vec<sys::AVHWDeviceType> {
    let mut found: Vec<sys::AVHWDeviceType> = Vec::new();
    unsafe {
        let mut t = sys::av_hwdevice_iterate_types(sys::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE);
        while t != sys::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
            found.push(t);
            t = sys::av_hwdevice_iterate_types(t);
        }
    }
    let mut ordered: Vec<sys::AVHWDeviceType> = Vec::new();
    for name in hw_device_type_priority_store().lock().unwrap().iter() {
        let c_name = CString::new(name.as_str()).expect("設定値のCString変換失敗");
        let device_type = unsafe { sys::av_hwdevice_find_type_by_name(c_name.as_ptr()) };
        if device_type != sys::AVHWDeviceType::AV_HWDEVICE_TYPE_NONE && found.contains(&device_type)
        {
            ordered.push(device_type);
        }
    }
    for t in found {
        if !ordered.contains(&t) {
            ordered.push(t);
        }
    }
    ordered
}

pub(crate) fn first_drm_render_node() -> Option<CString> {
    let dir = std::fs::read_dir("/dev/dri").ok()?;
    let mut nodes: Vec<String> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("renderD"))
        .collect();
    nodes.sort();
    let name = nodes.into_iter().next()?;
    CString::new(format!("/dev/dri/{name}")).ok()
}

unsafe fn config_supports_sw_format(
    hw_device_ctx: *mut sys::AVBufferRef,
    config: *const sys::AVCodecHWConfig,
    stream_sw_format: sys::AVPixelFormat,
) -> bool {
    unsafe {
        let stream_sw_format_i32 = std::mem::transmute::<sys::AVPixelFormat, i32>(stream_sw_format);
        let Some(target_sw_format) = resolve_hw_sw_format(stream_sw_format_i32) else {
            eprintln!(
                "[neoutl-video-decoder][diag] resolve_hw_sw_format失敗 stream_sw_format={stream_sw_format_i32}"
            );
            return false;
        };
        let want_sw_format = std::mem::transmute::<i32, sys::AVPixelFormat>(target_sw_format);
        eprintln!(
            "[neoutl-video-decoder][diag] config_supports_sw_format開始 config_pix_fmt={:?} want_sw_format={target_sw_format}",
            (*config).pix_fmt
        );

        let mut constraints = sys::av_hwdevice_get_hwframe_constraints(hw_device_ctx, ptr::null());
        if constraints.is_null() {
            eprintln!(
                "[neoutl-video-decoder][diag] av_hwdevice_get_hwframe_constraints=NULL(制約問い合わせ失敗) config_pix_fmt={:?} → 非対応扱いとしてフォールバック",
                (*config).pix_fmt
            );
            return false;
        }
        let valid_sw_formats = (*constraints).valid_sw_formats;
        let supported = if valid_sw_formats.is_null() {
            true
        } else {
            let mut p = valid_sw_formats;
            let mut found = false;
            let mut listed = Vec::new();
            while std::mem::transmute::<sys::AVPixelFormat, i32>(*p) != av_pix_fmt_none() {
                listed.push(std::mem::transmute::<sys::AVPixelFormat, i32>(*p));
                if *p == want_sw_format {
                    found = true;
                    break;
                }
                p = p.add(1);
            }
            eprintln!(
                "[neoutl-video-decoder][diag] valid_sw_formats={listed:?} want={target_sw_format} found={found}"
            );
            found
        };
        sys::av_hwframe_constraints_free(&mut constraints);
        supported
    }
}

pub(crate) unsafe fn try_init_hw_device(
    codec: *const sys::AVCodec,
    stream_sw_format: sys::AVPixelFormat,
    gpu_device: &Option<Arc<wgpu::Device>>,
    excluded_device_types: &HashSet<i32>,
) -> Option<(*mut sys::AVBufferRef, i32, i32)> {
    let _ = gpu_device;
    unsafe {
        let device_types = available_hw_device_types();
        eprintln!("[neoutl-video-decoder][diag] 検出HWデバイスタイプ={device_types:?}");
        if !excluded_device_types.is_empty() {
            eprintln!(
                "[neoutl-video-decoder][diag] poison済みのため除外するdevice_type={excluded_device_types:?}"
            );
        }
        eprintln!(
            "[neoutl-video-decoder][diag] env LIBVA_DRIVER_NAME={:?} WAYLAND_DISPLAY={:?} DISPLAY={:?}",
            std::env::var("LIBVA_DRIVER_NAME"),
            std::env::var("WAYLAND_DISPLAY"),
            std::env::var("DISPLAY")
        );
        let vaapi_render_node = first_drm_render_node();
        eprintln!(
            "[neoutl-video-decoder][diag] vaapi_render_node={:?}",
            vaapi_render_node.as_ref().map(|c| c.to_string_lossy())
        );

        for device_type in device_types {
            if excluded_device_types.contains(&std::mem::transmute::<sys::AVHWDeviceType, i32>(
                device_type,
            )) {
                continue;
            }
            let mut i = 0;
            loop {
                let config = sys::avcodec_get_hw_config(codec, i);
                if config.is_null() {
                    break;
                }
                let methods = (*config).methods;
                let matches_method =
                    (methods & sys::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32) != 0;
                if matches_method && (*config).device_type == device_type {
                    let device_arg = if device_type == sys::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI {
                        vaapi_render_node
                            .as_ref()
                            .map_or(ptr::null(), |c| c.as_ptr())
                    } else {
                        ptr::null()
                    };
                    let mut hw_device_ctx: *mut sys::AVBufferRef = ptr::null_mut();
                    let ret = sys::av_hwdevice_ctx_create(
                        &mut hw_device_ctx,
                        device_type,
                        device_arg,
                        ptr::null_mut(),
                        0,
                    );
                    eprintln!(
                        "[neoutl-video-decoder][diag] av_hwdevice_ctx_create device_type={device_type:?} ret={ret}"
                    );
                    if ret == 0 {
                        let format_ok =
                            config_supports_sw_format(hw_device_ctx, config, stream_sw_format);
                        if format_ok {
                            return Some((
                                hw_device_ctx,
                                std::mem::transmute::<sys::AVPixelFormat, i32>((*config).pix_fmt),
                                std::mem::transmute::<sys::AVHWDeviceType, i32>(device_type),
                            ));
                        }
                        sys::av_buffer_unref(&mut hw_device_ctx);
                    }
                }
                i += 1;
            }
        }
        eprintln!("[neoutl-video-decoder][diag] try_init_hw_device全候補探索終了、HW初期化失敗");
        None
    }
}
