use super::helpers::{
    ScanStatus, category_label, easing_engine_ids_and_names, index_of, load_from_disk, save_to_disk,
};
use crate::app::update::UpdateStatus;
use crate::audio::{plugin_registry, plugin_settings};
use crate::ecs::{
    EcsWorld,
    resources::{AudioPluginSettingsResource, CodecDecodeOverride, SystemSettingsResource},
};
use crate::infra::localization::tr;
use crate::ui::ui_ext::{self, UiExt, page_title, row_between_style};
use egui::{Context, Ui};
use egui_material_icons::{MaterialIcon, icons};
use egui_taffy::{TuiBuilderLogic, tui};
use elegance::{Accent, BuiltInTheme, Button};
use std::sync::{Arc, Mutex};

pub(super) const CATEGORIES: [(&str, MaterialIcon); 7] = [
    ("一般", icons::ICON_SETTINGS),
    ("外観", icons::ICON_PALETTE),
    ("パフォーマンス", icons::ICON_SPEED),
    ("デコード", icons::ICON_MOVIE),
    ("タイムライン", icons::ICON_VIEW_TIMELINE),
    ("音声プラグイン", icons::ICON_EXTENSION),
    ("アップデート", icons::ICON_SYSTEM_UPDATE),
];

pub struct SystemSettingsWindow {
    pub open: bool,
    pub(super) selected_category: i32,

    pub(super) theme_choice: BuiltInTheme,
    pub(super) easing_engine_ids: Vec<String>,
    pub(super) easing_engine_names: Vec<String>,
    pub(super) easing_engine_index: i32,

    pub(super) autosave_enabled: bool,
    pub(super) autosave_interval_sec: i32,
    pub(super) ui_scale_percent: i32,
    pub(super) worker_threads: i32,
    pub(super) audio_max_block_size: i32,
    pub(super) decode_backend: i32,
    pub(super) hw_decode_extra_frames: i32,
    pub(super) hw_device_type_priority: Vec<String>,
    pub(super) codec_decode_overrides: Vec<CodecDecodeOverride>,
    pub(super) selected_codec_tab: usize,
    pub(super) default_snap: bool,
    pub(super) magnetic_snap_range: i32,

    pub(super) check_update_on_startup: bool,
    pub(super) update_status: Arc<Mutex<UpdateStatus>>,
    pub(super) crash_reporting_enabled: bool,

    pub(super) audio_plugin_settings: AudioPluginSettingsResource,
    pub(super) new_scan_path: String,
    pub(super) scan_status: Arc<Mutex<ScanStatus>>,

    pub(super) save_status: String,
}

fn apply_codec_decode_overrides(overrides: &[CodecDecodeOverride]) {
    for o in overrides {
        neo_media_ffmpeg::set_force_sw_decode(&o.codec_kind, o.force_sw_decode);
        if o.custom_priority_enabled {
            neo_media_ffmpeg::set_hw_device_type_priority_for_codec(
                &o.codec_kind,
                o.hw_device_type_priority.clone(),
            );
        } else {
            neo_media_ffmpeg::clear_hw_device_type_priority_for_codec(&o.codec_kind);
        }
    }
}

impl SystemSettingsWindow {
    pub fn new(world_holder: &Arc<Mutex<EcsWorld>>) -> Self {
        if let Some(loaded) = load_from_disk() {
            world_holder.lock().unwrap().set_system_settings(loaded);
        }

        let (easing_engine_ids, easing_engine_names) = easing_engine_ids_and_names();
        let s = world_holder.lock().unwrap().get_system_settings();

        neoutl_media_runtime::runtime::set_worker_threads(s.worker_threads);
        neo_media_ffmpeg::set_decode_thread_cap(s.worker_threads);
        neo_media_ffmpeg::set_hw_decode_extra_frames(s.hw_decode_extra_frames);
        neo_media_ffmpeg::set_hw_device_type_priority(s.hw_device_type_priority.clone());
        apply_codec_decode_overrides(&s.codec_decode_overrides);
        crate::app::theme::restore(&s.theme_id);

        let update_status = Arc::new(Mutex::new(UpdateStatus::Idle));
        if s.check_update_on_startup {
            crate::app::update::spawn_check(update_status.clone());
        }

        let audio_plugin_settings = plugin_settings::load_from_disk().unwrap_or_default();
        plugin_registry::set_disabled(&audio_plugin_settings.disabled_plugin_ids);

        Self {
            open: false,
            selected_category: 0,
            theme_choice: crate::app::theme::current(),
            easing_engine_index: index_of(&easing_engine_ids, &s.easing_engine_id),
            easing_engine_ids,
            easing_engine_names,
            autosave_enabled: s.autosave_enabled,
            autosave_interval_sec: s.autosave_interval_sec,
            ui_scale_percent: s.ui_scale_percent,
            worker_threads: s.worker_threads,
            audio_max_block_size: s.audio_max_block_size,
            decode_backend: s.decode_backend,
            hw_decode_extra_frames: s.hw_decode_extra_frames,
            hw_device_type_priority: s.hw_device_type_priority.clone(),
            codec_decode_overrides: s.codec_decode_overrides.clone(),
            selected_codec_tab: 0,
            default_snap: s.default_snap,
            magnetic_snap_range: s.magnetic_snap_range,
            check_update_on_startup: s.check_update_on_startup,
            update_status,
            crash_reporting_enabled: s.crash_reporting_enabled,
            audio_plugin_settings,
            new_scan_path: String::new(),
            scan_status: Arc::new(Mutex::new(ScanStatus::Idle)),
            save_status: String::new(),
        }
    }

    pub(super) fn persist(
        &self,
        world_holder: &Arc<Mutex<EcsWorld>>,
        mutate: impl FnOnce(&mut SystemSettingsResource),
    ) {
        let mut world = world_holder.lock().unwrap();
        let mut s = world.get_system_settings();
        mutate(&mut s);
        world.set_system_settings(s);
    }

    pub(super) fn persist_codec_overrides(&self, world_holder: &Arc<Mutex<EcsWorld>>) {
        let overrides = self.codec_decode_overrides.clone();
        self.persist(world_holder, |s| {
            s.codec_decode_overrides = overrides.clone()
        });
    }

    pub(super) fn persistable_audio_plugin_settings(&self) -> AudioPluginSettingsResource {
        AudioPluginSettingsResource {
            cached_catalog: plugin_registry::get_all_unfiltered(),
            ..self.audio_plugin_settings.clone()
        }
    }

    pub(super) fn persist_audio_plugin_settings(&self) {
        let _ = plugin_settings::save_to_disk(&self.persistable_audio_plugin_settings());
        plugin_registry::set_disabled(&self.audio_plugin_settings.disabled_plugin_ids);
    }

    pub(super) fn reload(&mut self, world_holder: &Arc<Mutex<EcsWorld>>) {
        let Some(loaded) = load_from_disk() else {
            self.save_status = t!("設定ファイルなし");
            return;
        };
        world_holder
            .lock()
            .unwrap()
            .set_system_settings(loaded.clone());
        neoutl_media_runtime::runtime::set_worker_threads(loaded.worker_threads);
        neo_media_ffmpeg::set_decode_thread_cap(loaded.worker_threads);
        neo_media_ffmpeg::set_hw_decode_extra_frames(loaded.hw_decode_extra_frames);
        neo_media_ffmpeg::set_hw_device_type_priority(loaded.hw_device_type_priority.clone());
        apply_codec_decode_overrides(&loaded.codec_decode_overrides);
        self.hw_device_type_priority = loaded.hw_device_type_priority.clone();
        self.codec_decode_overrides = loaded.codec_decode_overrides.clone();

        self.theme_choice = crate::app::theme::from_id(&loaded.theme_id);
        crate::app::theme::set(self.theme_choice);
        self.easing_engine_index = index_of(&self.easing_engine_ids, &loaded.easing_engine_id);
        self.autosave_enabled = loaded.autosave_enabled;
        self.autosave_interval_sec = loaded.autosave_interval_sec;
        self.ui_scale_percent = loaded.ui_scale_percent;
        self.worker_threads = loaded.worker_threads;
        self.audio_max_block_size = loaded.audio_max_block_size;
        self.decode_backend = loaded.decode_backend;
        self.hw_decode_extra_frames = loaded.hw_decode_extra_frames;
        self.default_snap = loaded.default_snap;
        self.magnetic_snap_range = loaded.magnetic_snap_range;
        self.check_update_on_startup = loaded.check_update_on_startup;
        self.crash_reporting_enabled = loaded.crash_reporting_enabled;

        if let Some(loaded_plugins) = plugin_settings::load_from_disk() {
            self.audio_plugin_settings = loaded_plugins;
            plugin_registry::set_disabled(&self.audio_plugin_settings.disabled_plugin_ids);
        }

        self.save_status = t!("再読込完了");
    }

    pub fn show(&mut self, _ctx: &Context, ui: &mut Ui, world_holder: &Arc<Mutex<EcsWorld>>) {
        if !self.open {
            return;
        }

        egui::Panel::bottom("system_setting_footer").show(ui, |ui| {
            ui.footer_bar(|ui| {
                let row_width = ui.available_width();
                tui(ui, ui.id().with("system_settings_footer"))
                    .style(row_between_style(row_width))
                    .show(|tui| {
                        tui.ui(|ui| {
                            ui.label(&self.save_status);
                        });
                        tui.style(ui_ext::row_style(8.0)).add(|tui| {
                            tui.ui(|ui| {
                                if ui.add(Button::new(t!("保存"))).clicked() {
                                    let s = world_holder.lock().unwrap().get_system_settings();
                                    let plugin_save = plugin_settings::save_to_disk(
                                        &self.persistable_audio_plugin_settings(),
                                    );
                                    self.save_status = match (save_to_disk(&s), plugin_save) {
                                        (Ok(()), Ok(())) => t!("保存完了"),
                                        _ => t!("保存失敗"),
                                    };
                                }
                            });
                            tui.ui(|ui| {
                                if ui.add(Button::new(t!("再読込")).outline()).clicked() {
                                    self.reload(world_holder);
                                }
                            });
                        });
                    });
            })
        });

        egui::Panel::left("system_settings_categories")
            .resizable(false)
            .default_size(ui_ext::density().sidebar_panel_width())
            .frame(
                ui_ext::density()
                    .sidebar_frame(elegance::Theme::current(ui.ctx()).palette.input_bg),
            )
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                    for (i, (label, icon)) in CATEGORIES.iter().enumerate() {
                        self.category_item(ui, i as i32, label, icon);
                        ui.add_space(4.0);
                    }
                })
            });
        egui::Panel::top("system_setting_header").show(ui, |ui| {
            ui.header_bar(|ui| {
                ui.heading(page_title(tr(category_label(
                    self.selected_category as usize,
                ))));
            })
        });
        egui::CentralPanel::default().show(ui, |ui| {
            ui.page_content(|ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("system_settings_page")
                            .num_columns(2)
                            .spacing([10.0, 10.0])
                            .show(ui, |ui| match self.selected_category {
                                0 => self.page_general(ui, world_holder),
                                1 => self.page_appearance(ui, world_holder),
                                2 => self.page_performance(ui, world_holder),
                                3 => self.page_decode(ui, world_holder),
                                4 => self.page_timeline_defaults(ui, world_holder),
                                5 => self.page_audio_plugins(ui),
                                _ => self.page_update(ui, world_holder),
                            });
                        if self.selected_category == 3 {
                            self.page_decode_wide(ui, world_holder);
                        }
                    });
            });
        });
    }

    fn category_item(&mut self, ui: &mut Ui, index: i32, label: &str, icon: &MaterialIcon) {
        let active = index == self.selected_category;
        let is_update_category = index as usize == CATEGORIES.len() - 1;
        let has_update = is_update_category
            && matches!(
                *self.update_status.lock().unwrap(),
                UpdateStatus::Available(_)
            );
        let mark = if has_update { " ●" } else { "" };
        let text = format!("{}  {}{mark}", icon.codepoint, tr(label));

        let theme = elegance::Theme::current(ui.ctx());
        let p = &theme.palette;
        let size = egui::vec2(ui.available_width(), 32.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        if response.clicked() {
            self.selected_category = index;
        }
        if ui.is_rect_visible(rect) {
            let hovered = response.hovered();
            let fill = if active {
                p.accent_fill(Accent::Green)
            } else if hovered {
                egui::Color32::from_rgba_unmultiplied(
                    p.text_muted.r(),
                    p.text_muted.g(),
                    p.text_muted.b(),
                    15,
                )
            } else {
                p.input_bg
            };
            ui.painter().rect(
                rect,
                egui::CornerRadius::same(theme.control_radius as u8 + 2),
                fill,
                egui::Stroke::NONE,
                egui::StrokeKind::Inside,
            );
            let text_color = if active {
                egui::Color32::WHITE
            } else if hovered {
                p.text_muted
            } else {
                p.text_faint
            };
            ui.painter().text(
                egui::pos2(rect.left() + 12.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                text,
                egui::FontId::proportional(theme.typography.button),
                text_color,
            );
        }
    }
}
