//! 查找引用的结果卡片：列出每个引用的文件、行号和该行内容。
//!
//! 为了省内存，每行只用纯文本显示，不为每个引用创建编辑器。已打开文件的行从缓冲区取，
//! 其余文件在后台从磁盘读取。

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use lsp::types::{FileLocation, Location};
use lsp::ReferenceLocation;
use pathfinder_color::ColorU;
use string_offset::CharOffset;
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    Border, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, Fill, Flex, Hoverable, MouseStateHandle, ParentElement, Radius,
    ScrollbarWidth, Shrinkable, Text,
};
use warpui::keymap::FixedBinding;
use warpui::platform::Cursor;
use warpui::{AppContext, Element, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use super::global_buffer_model::GlobalBufferModel;

/// 卡片的最大高度。
pub const FIND_REFERENCES_MAX_HEIGHT: f32 = 300.;
/// 卡片宽度。卡片作为浮层显示，没有父级宽度约束，必须给定宽度，否则行内的
/// `Shrinkable` 会在无限宽度下布局导致崩溃。
const FIND_REFERENCES_WIDTH: f32 = 640.;
/// 最多显示的引用数，避免超大结果拖慢界面。
const MAX_REFERENCES: usize = 500;

pub fn init(app: &mut AppContext) {
    use warpui::keymap::macros::*;

    app.register_fixed_bindings([
        FixedBinding::new(
            "escape",
            FindReferencesAction::Close,
            id!(FindReferencesView::ui_name()),
        ),
        FixedBinding::new(
            "enter",
            FindReferencesAction::OpenSelected,
            id!(FindReferencesView::ui_name()),
        ),
        FixedBinding::new(
            "up",
            FindReferencesAction::Up,
            id!(FindReferencesView::ui_name()),
        ),
        FixedBinding::new(
            "down",
            FindReferencesAction::Down,
            id!(FindReferencesView::ui_name()),
        ),
    ]);
}

#[derive(Clone, Debug)]
pub enum FindReferencesAction {
    Open(usize),
    OpenSelected,
    Up,
    Down,
    Close,
}

pub enum FindReferencesEvent {
    Open(FileLocation),
    Close,
}

struct ReferenceEntry {
    location: FileLocation,
    file_name: String,
    /// `None` 表示还在读取。
    line_text: Option<String>,
    mouse_state: MouseStateHandle,
}

pub struct FindReferencesView {
    entries: Vec<ReferenceEntry>,
    selected: usize,
    /// 发起查找的位置（渲染偏移），卡片显示在这一行下方。
    anchor: CharOffset,
    scroll_state: ClippedScrollStateHandle,
}

impl FindReferencesView {
    pub fn new(
        references: Vec<ReferenceLocation>,
        anchor: CharOffset,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let mut entries: Vec<ReferenceEntry> = references
            .into_iter()
            .take(MAX_REFERENCES)
            .map(|reference| ReferenceEntry {
                file_name: reference
                    .file_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                location: FileLocation {
                    path: reference.file_path,
                    location: reference.range.start,
                },
                line_text: None,
                mouse_state: Default::default(),
            })
            .collect();

        // 已打开的文件直接从缓冲区取行，其余放到后台读。
        let mut to_read: HashMap<PathBuf, BTreeSet<usize>> = HashMap::new();
        for (path, lines) in lines_by_path(&entries) {
            let found = GlobalBufferModel::handle(ctx).update(ctx, |model, ctx| {
                model.get_lines_for_file(&path, lines.iter().copied().collect(), ctx)
            });
            match found {
                Some(found) => fill_lines(&mut entries, &path, found),
                None => {
                    to_read.insert(path, lines);
                }
            }
        }
        if !to_read.is_empty() {
            ctx.spawn(
                async move { read_lines_from_disk(to_read) },
                |me, results, ctx| {
                    for (path, lines) in results {
                        fill_lines(&mut me.entries, &path, lines);
                    }
                    ctx.notify();
                },
            );
        }

        Self {
            entries,
            selected: 0,
            anchor,
            scroll_state: Default::default(),
        }
    }

    pub fn anchor(&self) -> CharOffset {
        self.anchor
    }

    fn open(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        if let Some(entry) = self.entries.get(index) {
            ctx.emit(FindReferencesEvent::Open(entry.location.clone()));
        }
    }

    fn render_entry(
        &self,
        index: usize,
        entry: &ReferenceEntry,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let sub_color: ColorU = theme.sub_text_color(theme.background()).into();
        let font = appearance.monospace_font_family();
        let size = appearance.monospace_font_size();
        let file_name = entry.file_name.clone();
        let line_number = (entry.location.location.line + 1).to_string();
        let line_text = entry.line_text.clone().unwrap_or_else(|| "…".to_string());
        let is_selected = index == self.selected;
        let hover_fill = internal_colors::fg_overlay_2(theme);
        let text_color: ColorU = theme.active_ui_text_color().into();

        Hoverable::new(entry.mouse_state.clone(), move |state| {
            let row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    ConstrainedBox::new(
                        Text::new_inline(file_name, font, size)
                            .with_color(sub_color)
                            .finish(),
                    )
                    .with_width(140.)
                    .finish(),
                )
                .with_child(
                    ConstrainedBox::new(
                        Text::new_inline(line_number, font, size)
                            .with_color(sub_color)
                            .finish(),
                    )
                    .with_width(48.)
                    .finish(),
                )
                .with_child(
                    Shrinkable::new(
                        1.,
                        Text::new_inline(line_text, font, size)
                            .with_color(text_color)
                            .finish(),
                    )
                    .finish(),
                )
                .finish();
            let container = Container::new(row)
                .with_horizontal_padding(8.)
                .with_vertical_padding(4.);
            if state.is_hovered() || is_selected {
                container.with_background(hover_fill).finish()
            } else {
                container.finish()
            }
        })
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(FindReferencesAction::Open(index));
        })
        .with_cursor(Cursor::PointingHand)
        .finish()
    }
}

impl Entity for FindReferencesView {
    type Event = FindReferencesEvent;
}

impl View for FindReferencesView {
    fn ui_name() -> &'static str {
        "FindReferencesView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let title = if self.entries.is_empty() {
            "No references found".to_string()
        } else {
            format!("{} references", self.entries.len())
        };
        let header = Container::new(
            Text::new_inline(
                title,
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish(),
        )
        .with_horizontal_padding(8.)
        .with_vertical_padding(6.)
        .finish();

        let mut list = Flex::column().with_cross_axis_alignment(CrossAxisAlignment::Stretch);
        for (index, entry) in self.entries.iter().enumerate() {
            list.add_child(self.render_entry(index, entry, appearance));
        }
        let scrollable = ClippedScrollable::vertical(
            self.scroll_state.clone(),
            list.finish(),
            ScrollbarWidth::Auto,
            theme.disabled_ui_text_color().into(),
            theme.active_ui_text_color().into(),
            Fill::None,
        )
        .finish();

        let card = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(header)
            .with_child(Shrinkable::new(1., scrollable).finish())
            .finish();

        ConstrainedBox::new(
            Container::new(card)
                .with_background_color(internal_colors::neutral_1(theme))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .with_border(Border::all(1.).with_border_fill(internal_colors::neutral_4(theme)))
                .finish(),
        )
        .with_width(FIND_REFERENCES_WIDTH)
        .with_max_height(FIND_REFERENCES_MAX_HEIGHT)
        .finish()
    }
}

impl TypedActionView for FindReferencesView {
    type Action = FindReferencesAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            FindReferencesAction::Open(index) => self.open(*index, ctx),
            FindReferencesAction::OpenSelected => self.open(self.selected, ctx),
            FindReferencesAction::Up | FindReferencesAction::Down => {
                let len = self.entries.len();
                if len == 0 {
                    return;
                }
                self.selected = if matches!(action, FindReferencesAction::Up) {
                    (self.selected + len - 1) % len
                } else {
                    (self.selected + 1) % len
                };
                ctx.notify();
            }
            FindReferencesAction::Close => ctx.emit(FindReferencesEvent::Close),
        }
    }
}

/// 按文件分组需要的行号（从 0 开始）。
fn lines_by_path(entries: &[ReferenceEntry]) -> HashMap<PathBuf, BTreeSet<usize>> {
    let mut lines: HashMap<PathBuf, BTreeSet<usize>> = HashMap::new();
    for entry in entries {
        lines
            .entry(entry.location.path.clone())
            .or_default()
            .insert(entry.location.location.line);
    }
    lines
}

fn fill_lines(entries: &mut [ReferenceEntry], path: &Path, lines: Vec<(usize, String)>) {
    let lines: HashMap<usize, String> = lines.into_iter().collect();
    for entry in entries.iter_mut() {
        let Location { line, .. } = entry.location.location;
        if entry.location.path == path {
            if let Some(text) = lines.get(&line) {
                entry.line_text = Some(text.trim().to_string());
            }
        }
    }
}

fn read_lines_from_disk(
    to_read: HashMap<PathBuf, BTreeSet<usize>>,
) -> Vec<(PathBuf, Vec<(usize, String)>)> {
    to_read
        .into_iter()
        .filter_map(|(path, wanted)| {
            let content = std::fs::read_to_string(&path).ok()?;
            let lines = content
                .lines()
                .enumerate()
                .filter(|(index, _)| wanted.contains(index))
                .map(|(index, line)| (index, line.to_string()))
                .collect();
            Some((path, lines))
        })
        .collect()
}
