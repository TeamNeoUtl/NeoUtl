use ffmpeg_sys_next as sys;

pub const CODEC_KIND_LIST: &[&str] =
    &["av1", "hevc", "h264", "vp9", "vp8", "mpeg2", "mpeg4", "vc1"];

pub(crate) fn codec_kind_of(codec_id: sys::AVCodecID) -> Option<&'static str> {
    match codec_id {
        sys::AVCodecID::AV_CODEC_ID_AV1 => Some("av1"),
        sys::AVCodecID::AV_CODEC_ID_HEVC => Some("hevc"),
        sys::AVCodecID::AV_CODEC_ID_H264 => Some("h264"),
        sys::AVCodecID::AV_CODEC_ID_VP9 => Some("vp9"),
        sys::AVCodecID::AV_CODEC_ID_VP8 => Some("vp8"),
        sys::AVCodecID::AV_CODEC_ID_MPEG2VIDEO => Some("mpeg2"),
        sys::AVCodecID::AV_CODEC_ID_MPEG4 => Some("mpeg4"),
        sys::AVCodecID::AV_CODEC_ID_VC1 => Some("vc1"),
        _ => None,
    }
}
