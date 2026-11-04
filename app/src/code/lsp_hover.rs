//! 悬停提示（类型、文档和诊断信息）和 Cmd+点击跳转定义。
//!
//! - 鼠标停留 [`HOVER_DELAY`] 后才向服务器请求悬停信息，移动时取消未完成的请求。
//! - 按住 Cmd 悬停时请求定义位置，给符号加上可点击的链接，点击后跳转。

use std::ops::Range;
use std::time::Duration;

use futures::stream::AbortHandle;
use lsp::types::{DefinitionLocation, FileLocation};
use lsp::{HoverContents, HoverResult, MarkupKind};
use markdown_parser::{FormattedText, FormattedTextFragment, FormattedTextLine};
use pathfinder_geometry::vector::Vector2F;
use string_offset::CharOffset;
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    Border, ChildAnchor, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CornerRadius, Fill, FormattedTextElement, Hoverable, MouseStateHandle, OffsetPositioning,
    ParentAnchor, ParentOffsetBounds, Radius, ScrollbarWidth,
};
use warpui::r#async::Timer;
use warpui::{AppContext, Element, SingletonEntity, ViewContext};

use super::lsp_diagnostics::ProcessedDiagnostic;
use super::{LocalCodeEditorAction, LocalCodeEditorView};
use crate::code::editor::model::HoverableLink;

/// 鼠标停留多久后请求悬停信息。
const HOVER_DELAY: Duration = Duration::from_millis(500);
const HOVER_MAX_WIDTH: f32 = 400.;
const HOVER_MAX_HEIGHT: f32 = 200.;

#[derive(Default)]
pub(super) enum LspHoverState {
    #[default]
    None,
    /// 等待延迟或等待服务器返回。
    Loading(AbortHandle),
    Loaded {
        text: FormattedText,
        /// 悬停的符号范围（渲染偏移），用于定位和判断鼠标是否还在符号上。
        range: Range<CharOffset>,
        scroll_state: ClippedScrollStateHandle,
        mouse_state: MouseStateHandle,
    },
}

impl LspHoverState {
    /// 清除状态并取消未完成的请求。返回是否有变化。
    fn clear(&mut self) -> bool {
        if let Self::Loading(handle) = self {
            handle.abort();
        }
        let changed = !matches!(self, Self::None);
        *self = Self::None;
        changed
    }

    fn contains(&self, offset: CharOffset) -> bool {
        matches!(self, Self::Loaded { range, .. } if range.contains(&offset))
    }

    fn is_mouse_over_tooltip(&self) -> bool {
        let Self::Loaded { mouse_state, .. } = self else {
            return false;
        };
        mouse_state
            .lock()
            .is_ok_and(|state| state.is_mouse_over_element())
    }
}

impl LocalCodeEditorView {
    pub(super) fn handle_lsp_mouse_hover(
        &mut self,
        offset: CharOffset,
        cmd: bool,
        clamped: bool,
        is_covered: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.lsp_server.is_none() {
            return;
        }

        // 鼠标不在文字上，或被上层元素遮住。移到悬停窗口上时保留窗口。
        if clamped || is_covered {
            if self.lsp_hover.is_mouse_over_tooltip() {
                return;
            }
            let cleared_link = self
                .editor
                .update(ctx, |editor, ctx| editor.clear_hovered_symbol_range(ctx));
            if self.lsp_hover.clear() || cleared_link {
                ctx.notify();
            }
            return;
        }

        if cmd {
            self.fetch_definition_link(offset, ctx);
            return;
        }

        self.editor.update(ctx, |editor, ctx| {
            if editor.clear_hovered_symbol_range(ctx) {
                ctx.notify();
            }
        });
        if self.lsp_hover.contains(offset) {
            return;
        }
        if self.lsp_hover.clear() {
            ctx.notify();
        }
        let timer = async {
            Timer::after(HOVER_DELAY).await;
        };
        let handle = ctx.spawn(timer, move |me, _, ctx| me.request_hover(offset, ctx));
        self.lsp_hover = LspHoverState::Loading(handle.abort_handle());
    }

    fn request_hover(&mut self, offset: CharOffset, ctx: &mut ViewContext<Self>) {
        self.lsp_hover = LspHoverState::None;
        let (Some(server), Some(path)) = (self.lsp_server.as_ref(), self.file_path()) else {
            return;
        };
        let position = self.editor.as_ref(ctx).offset_to_lsp_position(offset, ctx);
        let future = match server.as_ref(ctx).hover(path.to_path_buf(), position) {
            Ok(future) => future,
            Err(e) => {
                log::debug!("Failed to request LSP hover: {e}");
                return;
            }
        };
        let handle = ctx.spawn(future, move |me, result, ctx| {
            me.show_hover(offset, result.ok().flatten(), ctx);
        });
        self.lsp_hover = LspHoverState::Loading(handle.abort_handle());
    }

    fn show_hover(
        &mut self,
        offset: CharOffset,
        hover: Option<HoverResult>,
        ctx: &mut ViewContext<Self>,
    ) {
        let mut lines: Vec<FormattedTextLine> = self
            .diagnostics_at(offset)
            .iter()
            .map(diagnostic_line)
            .collect();
        let hover_range = hover.as_ref().and_then(|hover| hover.range.clone());
        if let Some(hover) = hover.filter(|hover| !hover.contents.is_empty()) {
            if !lines.is_empty() {
                lines.push(FormattedTextLine::HorizontalRule);
            }
            lines.extend(hover_lines(hover.contents));
        }

        if lines.is_empty() {
            self.lsp_hover = LspHoverState::None;
        } else {
            let range = match hover_range {
                Some(range) => self.lsp_range_to_render(&range, ctx),
                None => self.word_render_range(offset, ctx),
            };
            self.lsp_hover = LspHoverState::Loaded {
                text: FormattedText::new(lines),
                range,
                scroll_state: Default::default(),
                mouse_state: Default::default(),
            };
        }
        ctx.notify();
    }

    /// 按住 Cmd 悬停：请求定义位置，把符号变成可点击的链接。
    fn fetch_definition_link(&mut self, offset: CharOffset, ctx: &mut ViewContext<Self>) {
        let editor = self.editor.as_ref(ctx);
        if editor
            .hovered_symbol_range(ctx)
            .is_some_and(|range| range.contains(&offset))
        {
            return;
        }
        let (Some(server), Some(path)) = (self.lsp_server.as_ref(), self.file_path()) else {
            return;
        };
        let position = editor.offset_to_lsp_position(offset, ctx);
        let future = match server
            .as_ref(ctx)
            .goto_definition(path.to_path_buf(), position)
        {
            Ok(future) => future,
            Err(e) => {
                log::debug!("Failed to request LSP definition: {e}");
                return;
            }
        };
        ctx.spawn(future, move |me, result, ctx| {
            let location = result
                .ok()
                .and_then(|locations| locations.into_iter().next());
            me.set_definition_link(offset, location, ctx);
        });
    }

    fn set_definition_link(
        &mut self,
        offset: CharOffset,
        location: Option<DefinitionLocation>,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(location) = location else {
            self.editor.update(ctx, |editor, ctx| {
                if editor.clear_hovered_symbol_range(ctx) {
                    ctx.notify();
                }
            });
            return;
        };

        let range = match &location.origin {
            Some(range) => self.lsp_range_to_render(range, ctx),
            None => self.word_render_range(offset, ctx),
        };
        let target = location.target;
        let window_id = ctx.window_id();
        let view_id = ctx.view_id();
        let link = HoverableLink::new(range).with_on_click(Box::new(move |app| {
            app.dispatch_typed_action_for_view(
                window_id,
                view_id,
                &LocalCodeEditorAction::NavigateToTarget(target.clone()),
            );
        }));
        self.editor.update(ctx, |editor, ctx| {
            if editor.set_hovered_symbol_range(Some(link), ctx) {
                ctx.notify();
            }
        });
    }

    /// 点击定义链接后跳转。0 起始的 LSP 行号由处理事件的一方换算。
    pub(super) fn navigate_to_definition(
        &mut self,
        target: &FileLocation,
        ctx: &mut ViewContext<Self>,
    ) {
        self.lsp_hover.clear();
        ctx.emit(super::LocalCodeEditorEvent::GotoDefinition {
            path: target.path.clone(),
            line: target.location.line,
            column: target.location.column,
        });
    }

    fn lsp_range_to_render(
        &self,
        range: &lsp::types::Range,
        ctx: &AppContext,
    ) -> Range<CharOffset> {
        let editor = self.editor.as_ref(ctx);
        let start = editor.lsp_location_to_offset(&range.start, ctx);
        let end = editor.lsp_location_to_offset(&range.end, ctx);
        to_render_offset(start)..to_render_offset(end)
    }

    fn word_render_range(&self, offset: CharOffset, ctx: &AppContext) -> Range<CharOffset> {
        match self.editor.as_ref(ctx).word_range_at_offset(offset, ctx) {
            Some(range) => to_render_offset(range.start)..to_render_offset(range.end),
            None => offset..offset + CharOffset::from(1),
        }
    }

    /// 渲染悬停窗口和它的位置。没有内容或符号不在可见区域时返回 `None`。
    pub(super) fn render_lsp_hover(
        &self,
        app: &AppContext,
    ) -> Option<(Box<dyn Element>, OffsetPositioning)> {
        let LspHoverState::Loaded {
            text,
            range,
            scroll_state,
            mouse_state,
        } = &self.lsp_hover
        else {
            return None;
        };
        let bounds = self
            .editor
            .as_ref(app)
            .character_bounds_in_viewport(range.start, app)?;

        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let content = FormattedTextElement::new(
            text.clone(),
            appearance.monospace_font_size(),
            appearance.ui_font_family(),
            appearance.monospace_font_family(),
            theme.active_ui_text_color().into(),
            Default::default(),
        )
        .finish();
        let scrollable = ClippedScrollable::vertical(
            scroll_state.clone(),
            content,
            ScrollbarWidth::Auto,
            theme.disabled_ui_text_color().into(),
            theme.active_ui_text_color().into(),
            Fill::None,
        )
        .finish();
        let tooltip = Container::new(
            ConstrainedBox::new(scrollable)
                .with_max_width(HOVER_MAX_WIDTH)
                .with_max_height(HOVER_MAX_HEIGHT)
                .finish(),
        )
        .with_horizontal_padding(8.)
        .with_vertical_padding(6.)
        .with_background(theme.background())
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
        .with_border(Border::all(1.).with_border_fill(internal_colors::neutral_4(theme)))
        .finish();
        let element = Hoverable::new(mouse_state.clone(), |_| tooltip).finish();

        // 上方空间够就显示在符号上方，否则显示在下方。
        let positioning = if bounds.origin_y() >= HOVER_MAX_HEIGHT {
            OffsetPositioning::offset_from_parent(
                Vector2F::new(bounds.origin_x(), bounds.origin_y()),
                ParentOffsetBounds::ParentByPosition,
                ParentAnchor::TopLeft,
                ChildAnchor::BottomLeft,
            )
        } else {
            OffsetPositioning::offset_from_parent(
                Vector2F::new(bounds.origin_x(), bounds.max_y()),
                ParentOffsetBounds::ParentByPosition,
                ParentAnchor::TopLeft,
                ChildAnchor::TopLeft,
            )
        };
        Some((element, positioning))
    }
}

/// 缓冲区偏移从 1 开始，渲染偏移从 0 开始。
fn to_render_offset(offset: CharOffset) -> CharOffset {
    let one = CharOffset::from(1);
    if offset > one {
        offset - one
    } else {
        CharOffset::from(0)
    }
}

fn diagnostic_line(diagnostic: &ProcessedDiagnostic) -> FormattedTextLine {
    let label = if diagnostic.is_error {
        "Error: "
    } else {
        "Warning: "
    };
    FormattedTextLine::Line(vec![
        FormattedTextFragment::bold(label),
        FormattedTextFragment::plain_text(diagnostic.message.clone()),
    ])
}

fn hover_lines(contents: HoverContents) -> Vec<FormattedTextLine> {
    contents
        .sections
        .into_iter()
        .flat_map(|section| {
            let parsed = match section.kind {
                MarkupKind::Markdown => markdown_parser::parse_markdown(&section.value).ok(),
                MarkupKind::PlainText => None,
            };
            match parsed {
                Some(text) => text.lines.into_iter().collect::<Vec<_>>(),
                None => vec![FormattedTextLine::Line(vec![
                    FormattedTextFragment::plain_text(section.value),
                ])],
            }
        })
        .collect()
}
