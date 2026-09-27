use super::fields::{choice_field, int_field};
use super::helpers::{codec_display_name, hw_backend_display_name};
use super::window::SystemSettingsWindow;
use crate::ecs::EcsWorld;
use crate::ecs::resources::CodecDecodeOverride;
use crate::infra::localization::tr;
use std::sync::{Arc, Mutex};

/// SortableListによるバックエンド優先順編集の共通ロジック。
/// priorityの内容が変化した場合にtrueを返す。呼び出し側で永続化・
/// neo_media_ffmpeg反映を行う。
/// スクロールはページ全体の外側ScrollAreaに一本化し、内側に
/// 独立したScrollAreaは持たせない（入れ子スクロールによる
/// 高さ計算崩れ・ホイールイベント競合を避けるため）。
fn priority_editor(ui: &mut egui::Ui, id_prefix: &str, priority: &mut Vec<String>) -> bool {
    let mut rows: Vec<elegance::SortableItem> = priority
        .iter()
        .map(|id| elegance::SortableItem::new(id.clone(), hw_backend_display_name(id)))
        .collect();
    elegance::SortableList::new(id_prefix, &mut rows).show(ui);
    let updated: Vec<String> = rows.into_iter().map(|row| row.id).collect();
    if updated != *priority {
        *priority = updated;
        true
    } else {
        false
    }
}

impl SystemSettingsWindow {
    pub(super) fn page_performance(
        &mut self,
        ui: &mut egui::Ui,
        world_holder: &Arc<Mutex<EcsWorld>>,
    ) {
        let mut worker_threads = self.worker_threads;
        let mut audio_max_block_size = self.audio_max_block_size;
        let changed = int_field(
            ui,
            "ワーカースレッド数（0=自動）",
            &mut worker_threads,
            0,
            64,
        ) | int_field(
            ui,
            "オーディオ最大ブロックサイズ",
            &mut audio_max_block_size,
            64,
            16384,
        );
        if changed {
            self.worker_threads = worker_threads;
            self.audio_max_block_size = audio_max_block_size;
            let (threads, block) = (self.worker_threads, self.audio_max_block_size);
            self.persist(world_holder, |s| {
                s.worker_threads = threads;
                s.audio_max_block_size = block;
            });
            neoutl_media_runtime::runtime::set_worker_threads(threads);
            neo_media_ffmpeg::set_decode_thread_cap(threads);
        }
    }

    pub(super) fn page_decode(&mut self, ui: &mut egui::Ui, world_holder: &Arc<Mutex<EcsWorld>>) {
        debug_assert_eq!(crate::project::config::DECODE_BACKEND_AUTO, 0);
        debug_assert_eq!(crate::project::config::DECODE_BACKEND_GPU_FIXED, 1);
        debug_assert_eq!(crate::project::config::DECODE_BACKEND_CPU_FIXED, 2);
        let options = [
            "自動".to_string(),
            "GPU固定".to_string(),
            "CPU固定".to_string(),
        ];
        let mut decode_backend = self.decode_backend;
        if choice_field(
            ui,
            "映像デコードバックエンド",
            &options,
            &mut decode_backend,
        ) {
            self.decode_backend = decode_backend;
            self.persist(world_holder, |s| s.decode_backend = decode_backend);
        }

        let mut hw_decode_extra_frames = self.hw_decode_extra_frames;
        if int_field(
            ui,
            "HWデコードサーフェス予備数",
            &mut hw_decode_extra_frames,
            crate::project::config::HW_DECODE_EXTRA_FRAMES_MIN,
            crate::project::config::HW_DECODE_EXTRA_FRAMES_MAX,
        ) {
            self.hw_decode_extra_frames = hw_decode_extra_frames;
            self.persist(world_holder, |s| {
                s.hw_decode_extra_frames = hw_decode_extra_frames
            });
            neo_media_ffmpeg::set_hw_decode_extra_frames(hw_decode_extra_frames);
        }
    }

    pub(super) fn page_decode_wide(
        &mut self,
        ui: &mut egui::Ui,
        world_holder: &Arc<Mutex<EcsWorld>>,
    ) {
        ui.separator();
        ui.add_space(8.0);
        ui.label(tr("HWデコードバックエンド優先順"));
        ui.add_space(4.0);

        let mut priority = self.hw_device_type_priority.clone();
        if priority_editor(ui, "hw_device_type_priority", &mut priority) {
            self.hw_device_type_priority = priority.clone();
            self.persist(world_holder, |s| {
                s.hw_device_type_priority = priority.clone()
            });
            neo_media_ffmpeg::set_hw_device_type_priority(priority);
        }

        ui.add_space(8.0);
        if ui.button(t!("既定順に戻す")).clicked() {
            let defaults = neo_media_ffmpeg::default_hw_device_type_priority();
            self.hw_device_type_priority = defaults.clone();
            self.persist(world_holder, |s| {
                s.hw_device_type_priority = defaults.clone()
            });
            neo_media_ffmpeg::set_hw_device_type_priority(defaults);
        }

        self.page_decode_codec_overrides(ui, world_holder);
    }

    fn codec_override_index(&mut self, codec_kind: &str) -> usize {
        if let Some(i) = self
            .codec_decode_overrides
            .iter()
            .position(|o| o.codec_kind == codec_kind)
        {
            return i;
        }
        self.codec_decode_overrides.push(CodecDecodeOverride {
            codec_kind: codec_kind.to_owned(),
            force_sw_decode: false,
            custom_priority_enabled: false,
            hw_device_type_priority: self.hw_device_type_priority.clone(),
        });
        self.codec_decode_overrides.len() - 1
    }

    fn page_decode_codec_overrides(
        &mut self,
        ui: &mut egui::Ui,
        world_holder: &Arc<Mutex<EcsWorld>>,
    ) {
        ui.separator();
        ui.add_space(8.0);
        ui.label(tr("コーデック別デコード設定"));
        ui.add_space(4.0);

        for codec_kind in neo_media_ffmpeg::CODEC_KIND_LIST {
            let idx = self.codec_override_index(codec_kind);

            ui.add_space(6.0);
            ui.label(codec_display_name(codec_kind));

            let mut force_sw = self.codec_decode_overrides[idx].force_sw_decode;
            if ui
                .add(elegance::Switch::new(&mut force_sw, tr("SWデコード強制")))
                .changed()
            {
                self.codec_decode_overrides[idx].force_sw_decode = force_sw;
                self.persist_codec_overrides(world_holder);
                neo_media_ffmpeg::set_force_sw_decode(codec_kind, force_sw);
            }

            ui.add_enabled_ui(!force_sw, |ui| {
                let mut custom = self.codec_decode_overrides[idx].custom_priority_enabled;
                if ui
                    .add(elegance::Switch::new(
                        &mut custom,
                        tr("個別バックエンド優先順を使用"),
                    ))
                    .changed()
                {
                    self.codec_decode_overrides[idx].custom_priority_enabled = custom;
                    self.persist_codec_overrides(world_holder);
                    if custom {
                        neo_media_ffmpeg::set_hw_device_type_priority_for_codec(
                            codec_kind,
                            self.codec_decode_overrides[idx]
                                .hw_device_type_priority
                                .clone(),
                        );
                    } else {
                        neo_media_ffmpeg::clear_hw_device_type_priority_for_codec(codec_kind);
                    }
                }
            });
        }

        self.page_decode_codec_priority_tabs(ui, world_holder);
    }

    /// custom_priority_enabledが1つ以上のコーデックについてのみtabbarを表示し、
    /// 選択中のコーデック1つ分のSortableListのみ描画する。
    fn page_decode_codec_priority_tabs(
        &mut self,
        ui: &mut egui::Ui,
        world_holder: &Arc<Mutex<EcsWorld>>,
    ) {
        let enabled_codecs: Vec<&'static str> = neo_media_ffmpeg::CODEC_KIND_LIST
            .iter()
            .copied()
            .filter(|k| {
                self.codec_decode_overrides
                    .iter()
                    .any(|o| o.codec_kind == *k && o.custom_priority_enabled)
            })
            .collect();

        if enabled_codecs.is_empty() {
            return;
        }
        if self.selected_codec_tab >= enabled_codecs.len() {
            self.selected_codec_tab = 0;
        }

        ui.add_space(8.0);
        let labels: Vec<String> = enabled_codecs
            .iter()
            .map(|k| codec_display_name(k))
            .collect();
        ui.add(elegance::TabBar::new(&mut self.selected_codec_tab, labels));
        ui.add_space(4.0);

        let codec_kind = enabled_codecs[self.selected_codec_tab];
        let idx = self.codec_override_index(codec_kind);
        let mut order = self.codec_decode_overrides[idx]
            .hw_device_type_priority
            .clone();
        if priority_editor(ui, codec_kind, &mut order) {
            self.codec_decode_overrides[idx].hw_device_type_priority = order.clone();
            self.persist_codec_overrides(world_holder);
            neo_media_ffmpeg::set_hw_device_type_priority_for_codec(codec_kind, order);
        }
    }
}
