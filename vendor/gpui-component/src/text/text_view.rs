use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Bounds, ClipboardItem, Context, Element, ElementId, Entity,
    EntityId, FocusHandle, GlobalElementId, InspectorElementId, InteractiveElement, IntoElement,
    KeyBinding, LayoutId, ListState, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    ListOffset, Pixels, Point, RenderOnce, SharedString, Size, StyleRefinement, Styled, Timer,
    Window, div, point, px,
};
use smol::stream::StreamExt;

use crate::highlighter::HighlightTheme;
use crate::scroll::{ScrollableElement, ScrollbarHandle};
use crate::text::node::CodeBlock;
use crate::{ActiveTheme, StyledExt, v_flex};
use crate::{
    global_state::GlobalState,
    input::{self},
    text::{
        TextViewStyle,
        node::{self, NodeContext},
    },
};

const CONTEXT: &'static str = "TextView";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(vec![
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", input::Copy, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-c", input::Copy, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-a", input::SelectAll, Some(CONTEXT)),
        KeyBinding::new("ctrl-a", input::SelectAll, Some(CONTEXT)),
    ]);
}

#[derive(IntoElement, Clone)]
struct TextViewElement {
    list_state: Option<ListState>,
    state: Entity<TextViewState>,
}

impl RenderOnce for TextViewElement {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, cx| {
            v_flex()
                .size_full()
                .map(|this| match &mut state.parsed_result {
                    Some(Ok(content)) => this.child(content.root_node.render_root(
                        self.list_state.clone(),
                        &content.node_cx,
                        window,
                        cx,
                    )),
                    Some(Err(err)) => this.child(
                        v_flex()
                            .gap_1()
                            .child("Failed to parse content")
                            .child(err.to_string()),
                    ),
                    None => this,
                })
        })
    }
}

/// Type for code block actions generator function.
pub(crate) type CodeBlockActionsFn =
    dyn Fn(&CodeBlock, &mut Window, &mut App) -> AnyElement + Send + Sync;

/// A text view that can render Markdown or HTML.
///
/// ## Goals
///
/// - Provide a rich text rendering component for such as Markdown or HTML,
/// used to display rich text in GPUI application (e.g., Help messages, Release notes)
/// - Support Markdown GFM and HTML (Simple HTML like Safari Reader Mode) for showing most common used markups.
/// - Support Heading, Paragraph, Bold, Italic, StrikeThrough, Code, Link, Image, Blockquote, List, Table, HorizontalRule, CodeBlock ...
///
/// ## Not Goals
///
/// - Customization of the complex style (some simple styles will be supported)
/// - As a Markdown editor or viewer (If you want to like this, you must fork your version).
/// - As a HTML viewer, we not support CSS, we only support basic HTML tags for used to as a content reader.
///
/// See also [`MarkdownElement`], [`HtmlElement`]
#[derive(Clone)]
pub struct TextView {
    id: ElementId,
    init_state: Option<InitState>,
    raw: SharedString,
    state: Entity<TextViewState>,
    style: StyleRefinement,
    selectable: bool,
    scrollable: bool,
    select_all_text: Option<SharedString>,
    code_block_actions: Option<Arc<CodeBlockActionsFn>>,
}

#[derive(PartialEq)]
pub(crate) struct ParsedContent {
    pub(crate) root_node: node::Node,
    pub(crate) node_cx: node::NodeContext,
}

/// The type of the text view.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextViewType {
    /// Markdown view
    Markdown,
    /// HTML view
    Html,
}

enum Update {
    Text(SharedString),
    Style(Box<TextViewStyle>),
}

struct ParsedUpdate {
    text: SharedString,
    result: Result<ParsedContent, SharedString>,
}

#[derive(Default)]
struct EstimatedScrollMetrics {
    row_weights: Vec<usize>,
    line_height: Pixels,
    dragging: bool,
}

/// ListState only includes measured rows in its scrollbar range. TextView
/// deliberately virtualizes large documents, so that range changes as rows
/// enter the viewport and makes the thumb jump during a drag. This adapter
/// derives a stable range from the parsed block line counts instead.
#[derive(Clone)]
struct TextViewScrollHandle {
    list_state: ListState,
    metrics: Rc<RefCell<EstimatedScrollMetrics>>,
}

impl TextViewScrollHandle {
    fn new(list_state: ListState) -> Self {
        Self {
            list_state,
            metrics: Rc::new(RefCell::new(EstimatedScrollMetrics::default())),
        }
    }

    fn set_line_height(&self, line_height: Pixels) {
        self.metrics.borrow_mut().line_height = line_height;
    }

    fn set_row_weights(&self, row_weights: Vec<usize>) {
        self.metrics.borrow_mut().row_weights = row_weights;
    }

    fn is_dragging(&self) -> bool {
        self.metrics.borrow().dragging
    }

    fn total_height(&self) -> Pixels {
        let metrics = self.metrics.borrow();
        let lines = metrics.row_weights.iter().copied().sum::<usize>().max(1);
        metrics.line_height * lines as f32
    }

    fn max_scroll(&self) -> Pixels {
        (self.total_height() - self.list_state.viewport_bounds().size.height).max(px(0.))
    }

    fn pixel_offset(&self) -> Pixels {
        let metrics = self.metrics.borrow();
        let logical = self.list_state.logical_scroll_top();
        let preceding_lines = metrics
            .row_weights
            .iter()
            .take(logical.item_ix)
            .copied()
            .sum::<usize>();
        let offset = metrics.line_height * preceding_lines as f32 + logical.offset_in_item;
        drop(metrics);
        offset.min(self.max_scroll()).max(px(0.))
    }

    fn normalized_offset(&self) -> f32 {
        let max = self.max_scroll();
        if max <= px(0.) {
            0.
        } else {
            (self.pixel_offset() / max).clamp(0., 1.)
        }
    }

    fn set_normalized_offset(&self, normalized: f32) {
        let y = -self.max_scroll() * normalized.clamp(0., 1.);
        self.set_offset(point(px(0.), y));
    }
}

impl ScrollbarHandle for TextViewScrollHandle {
    fn offset(&self) -> Point<Pixels> {
        point(px(0.), -self.pixel_offset())
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let target = (-offset.y).clamp(px(0.), self.max_scroll());
        let metrics = self.metrics.borrow();
        if metrics.row_weights.is_empty() {
            self.list_state.scroll_to(ListOffset::default());
            return;
        }

        let mut top = px(0.);
        let mut item_ix = 0;
        for (ix, weight) in metrics.row_weights.iter().copied().enumerate() {
            let row_height = metrics.line_height * weight.max(1) as f32;
            if top + row_height > target {
                item_ix = ix;
                break;
            }
            top += row_height;
            item_ix = (ix + 1).min(metrics.row_weights.len().saturating_sub(1));
        }
        self.list_state.scroll_to(ListOffset {
            item_ix,
            offset_in_item: (target - top).max(px(0.)),
        });
    }

    fn content_size(&self) -> Size<Pixels> {
        let viewport = self.list_state.viewport_bounds().size;
        Size::new(viewport.width, self.total_height().max(viewport.height))
    }

    fn start_drag(&self) {
        self.metrics.borrow_mut().dragging = true;
    }

    fn end_drag(&self) {
        self.metrics.borrow_mut().dragging = false;
    }
}

struct UpdateFuture {
    type_: TextViewType,
    highlight_theme: Arc<HighlightTheme>,
    current_style: TextViewStyle,
    current_text: SharedString,
    timer: Timer,
    rx: Pin<Box<smol::channel::Receiver<Update>>>,
    tx_result: smol::channel::Sender<ParsedUpdate>,
    delay: Duration,
    code_block_actions: Option<Arc<CodeBlockActionsFn>>,
}

impl UpdateFuture {
    #[allow(clippy::too_many_arguments)]
    fn new(
        type_: TextViewType,
        style: TextViewStyle,
        text: SharedString,
        highlight_theme: Arc<HighlightTheme>,
        rx: smol::channel::Receiver<Update>,
        tx_result: smol::channel::Sender<ParsedUpdate>,
        delay: Duration,
        code_block_actions: Option<Arc<CodeBlockActionsFn>>,
    ) -> Self {
        Self {
            type_,
            highlight_theme,
            current_style: style,
            current_text: text,
            timer: Timer::never(),
            rx: Box::pin(rx),
            tx_result,
            delay,
            code_block_actions,
        }
    }
}

impl Future for UpdateFuture {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.rx.poll_next(cx) {
                Poll::Ready(Some(update)) => {
                    let changed = match update {
                        Update::Text(text) if self.current_text != text => {
                            self.current_text = text;
                            true
                        }
                        Update::Style(style) if self.current_style != *style => {
                            self.highlight_theme = style.highlight_theme.clone();
                            self.current_style = *style;
                            true
                        }
                        _ => false,
                    };
                    if changed {
                        let delay = self.delay;
                        self.timer.set_after(delay);
                    }
                    continue;
                }
                Poll::Ready(None) => return Poll::Ready(()),
                Poll::Pending => {}
            }

            match self.timer.poll_next(cx) {
                Poll::Ready(Some(_)) => {
                    let result = parse_content(
                        self.type_,
                        &self.current_text,
                        self.current_style.clone(),
                        &self.highlight_theme,
                        &self.code_block_actions.clone(),
                    );
                    _ = self.tx_result.try_send(ParsedUpdate {
                        text: self.current_text.clone(),
                        result,
                    });
                    continue;
                }
                Poll::Ready(None) | Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Clone)]
enum InitState {
    Initializing {
        type_: TextViewType,
        text: SharedString,
        style: Box<TextViewStyle>,
        highlight_theme: Arc<HighlightTheme>,
    },
    Initialized {
        tx: smol::channel::Sender<Update>,
    },
}

pub(crate) struct TextViewState {
    parent_entity: Option<EntityId>,
    tx: Option<smol::channel::Sender<Update>>,
    parsed_result: Option<Result<ParsedContent, SharedString>>,
    parsed_text: Option<SharedString>,
    focus_handle: Option<FocusHandle>,
    /// The bounds of the text view
    bounds: Bounds<Pixels>,
    /// The local (in TextView) position of the selection.
    selection_positions: (Option<Point<Pixels>>, Option<Point<Pixels>>),
    /// Is current in selection.
    is_selecting: bool,
    selection_needs_layout: bool,
    is_all_selected: bool,
    is_selectable: bool,
    select_all_text: Option<SharedString>,
    list_state: ListState,
    scroll_handle: TextViewScrollHandle,
    pending_parsed_update: Option<ParsedUpdate>,
}

impl TextViewState {
    fn new(cx: &mut Context<TextViewState>) -> Self {
        let focus_handle = cx.focus_handle();
        let list_state = ListState::new(0, gpui::ListAlignment::Top, px(1000.));
        let scroll_handle = TextViewScrollHandle::new(list_state.clone());
        Self {
            parent_entity: None,
            tx: None,
            parsed_result: None,
            parsed_text: None,
            focus_handle: Some(focus_handle),
            bounds: Bounds::default(),
            selection_positions: (None, None),
            is_selecting: false,
            selection_needs_layout: false,
            is_all_selected: false,
            is_selectable: false,
            select_all_text: None,
            list_state,
            scroll_handle,
            pending_parsed_update: None,
        }
    }
}

impl TextViewState {
    fn install_parsed_update(&mut self, parsed_update: ParsedUpdate) {
        let old_count = self.list_state.item_count();
        let old_position = self.scroll_handle.normalized_offset();
        let is_append = self
            .parsed_text
            .as_ref()
            .is_some_and(|old| parsed_update.text.starts_with(old.as_ref()));
        let row_weights = parsed_update
            .result
            .as_ref()
            .ok()
            .map(|content| content.root_node.root_scroll_weights())
            .unwrap_or_default();
        let new_count = row_weights.len();

        if is_append && new_count >= old_count {
            self.list_state
                .splice(old_count..old_count, new_count - old_count);
        } else {
            self.list_state.reset(new_count);
        }
        self.scroll_handle.set_row_weights(row_weights);
        if !is_append && old_count > 0 {
            self.scroll_handle.set_normalized_offset(old_position);
        }

        self.parsed_result = Some(parsed_update.result);
        self.parsed_text = Some(parsed_update.text);
        self.clear_selection();
    }

    fn apply_pending_parsed_update(&mut self) {
        if !self.scroll_handle.is_dragging()
            && let Some(update) = self.pending_parsed_update.take()
        {
            self.install_parsed_update(update);
        }
    }

    /// Save bounds and unselect if bounds changed.
    fn update_bounds(&mut self, bounds: Bounds<Pixels>) {
        if self.bounds.size != bounds.size {
            self.clear_selection();
        }
        self.bounds = bounds;
    }

    fn clear_selection(&mut self) {
        if let Some(Ok(content)) = &self.parsed_result {
            content.root_node.clear_selection();
        }
        self.selection_positions = (None, None);
        self.is_selecting = false;
        self.selection_needs_layout = false;
        self.is_all_selected = false;
    }

    fn start_selection(&mut self, pos: Point<Pixels>) {
        self.clear_selection();
        let pos = pos - self.bounds.origin;
        self.selection_positions = (Some(pos), Some(pos));
        self.is_selecting = true;
    }

    fn update_selection(&mut self, pos: Point<Pixels>) {
        let pos = pos - self.bounds.origin;
        if let (Some(start), Some(_)) = self.selection_positions {
            self.selection_positions = (Some(start), Some(pos))
        }
    }

    fn end_selection(&mut self) {
        self.is_selecting = false;
        self.selection_needs_layout = self.has_selection();
    }

    fn finish_selection_layout(&mut self) {
        self.selection_needs_layout = false;
    }

    fn select_all(&mut self) {
        self.clear_selection();
        self.is_all_selected = true;
    }

    pub(crate) fn has_selection(&self) -> bool {
        if self.is_all_selected {
            return true;
        }
        if let (Some(start), Some(end)) = self.selection_positions {
            start != end
        } else {
            false
        }
    }

    pub(crate) fn is_selectable(&self) -> bool {
        self.is_selectable
    }

    pub(crate) fn is_selecting(&self) -> bool {
        self.is_selecting
    }

    pub(crate) fn selection_needs_layout(&self) -> bool {
        self.selection_needs_layout
    }

    pub(crate) fn is_all_selected(&self) -> bool {
        self.is_all_selected
    }

    /// Return the bounds of the selection in window coordinates.
    pub(crate) fn selection_bounds(&self) -> Bounds<Pixels> {
        selection_bounds(
            self.selection_positions.0,
            self.selection_positions.1,
            self.bounds,
        )
    }

    fn selection_text(&self) -> Option<String> {
        if self.is_all_selected {
            return self.select_all_text.as_ref().map(ToString::to_string);
        }
        Some(
            self.parsed_result
                .as_ref()?
                .as_ref()
                .ok()?
                .root_node
                .selected_text(),
        )
    }
}

#[derive(IntoElement, Clone)]
pub enum Text {
    String(SharedString),
    TextView(Box<TextView>),
}

impl From<SharedString> for Text {
    fn from(s: SharedString) -> Self {
        Self::String(s)
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Self::String(SharedString::from(s.to_string()))
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Self::String(s.into())
    }
}

impl From<TextView> for Text {
    fn from(e: TextView) -> Self {
        Self::TextView(Box::new(e))
    }
}

impl Text {
    /// Set the style for [`TextView`].
    ///
    /// Do nothing if this is `String`.
    pub fn style(self, style: TextViewStyle) -> Self {
        match self {
            Self::String(s) => Self::String(s),
            Self::TextView(e) => Self::TextView(Box::new(e.style(style))),
        }
    }

    /// Get the str
    pub fn as_str(&self) -> &str {
        match self {
            Self::String(s) => s.as_str(),
            Self::TextView(view) => view.raw.as_str(),
        }
    }
}

impl RenderOnce for Text {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        match self {
            Self::String(s) => s.into_any_element(),
            Self::TextView(e) => e.into_any_element(),
        }
    }
}

impl Styled for TextView {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl TextView {
    fn create_init_state(
        type_: TextViewType,
        text: &SharedString,
        highlight_theme: &Arc<HighlightTheme>,
        state: &Entity<TextViewState>,
        cx: &mut App,
    ) -> InitState {
        let state = state.read(cx);
        if let Some(tx) = &state.tx {
            InitState::Initialized { tx: tx.clone() }
        } else {
            InitState::Initializing {
                type_,
                text: text.clone(),
                style: Default::default(),
                highlight_theme: highlight_theme.clone(),
            }
        }
    }

    /// Create a new markdown text view.
    pub fn markdown(
        id: impl Into<ElementId>,
        markdown: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let id: ElementId = id.into();
        let markdown = markdown.into();
        let highlight_theme = cx.theme().highlight_theme.clone();
        let state =
            window.use_keyed_state(SharedString::from(format!("{}/state", id)), cx, |_, cx| {
                TextViewState::new(cx)
            });
        let init_state = Self::create_init_state(
            TextViewType::Markdown,
            &markdown,
            &highlight_theme,
            &state,
            cx,
        );
        if let Some(tx) = &state.read(cx).tx {
            let _ = tx.try_send(Update::Text(markdown.clone()));
        }
        Self {
            id,
            init_state: Some(init_state),
            raw: markdown.clone(),
            style: StyleRefinement::default(),
            state,
            selectable: false,
            scrollable: false,
            select_all_text: None,
            code_block_actions: None,
        }
    }

    /// Create a new html text view.
    pub fn html(
        id: impl Into<ElementId>,
        html: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let id: ElementId = id.into();
        let html = html.into();
        let highlight_theme = cx.theme().highlight_theme.clone();
        let state =
            window.use_keyed_state(SharedString::from(format!("{}/state", id)), cx, |_, cx| {
                TextViewState::new(cx)
            });
        let init_state =
            Self::create_init_state(TextViewType::Html, &html, &highlight_theme, &state, cx);
        if let Some(tx) = &state.read(cx).tx {
            let _ = tx.try_send(Update::Text(html.clone()));
        }
        Self {
            id,
            init_state: Some(init_state),
            style: StyleRefinement::default(),
            state,
            raw: html,
            selectable: false,
            scrollable: false,
            select_all_text: None,
            code_block_actions: None,
        }
    }

    /// Set the source text of the text view.
    pub fn text(mut self, raw: impl Into<SharedString>) -> Self {
        let raw: SharedString = raw.into();
        if let Some(init_state) = &mut self.init_state {
            match init_state {
                InitState::Initializing { text, .. } => *text = raw.clone(),
                InitState::Initialized { tx } => {
                    let _ = tx.try_send(Update::Text(raw.clone()));
                }
            }
        }
        self.raw = raw;
        self
    }

    /// Set [`TextViewStyle`].
    pub fn style(mut self, style: TextViewStyle) -> Self {
        if let Some(init_state) = &mut self.init_state {
            match init_state {
                InitState::Initializing { style: s, .. } => **s = style,
                InitState::Initialized { tx } => {
                    let _ = tx.try_send(Update::Style(Box::new(style)));
                }
            }
        }
        self
    }

    /// Set the text view to be selectable, default is false.
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }

    /// Set the text view to be scrollable, default is false.
    ///
    /// ## If true for `scrollable`
    ///
    /// The `scrollable` mode used for large content,
    /// will show scrollbar, but requires the parent to have a fixed height,
    /// and use [`gpui::list`] to render the content in a virtualized way.
    ///
    /// ## If false to fit content
    ///
    /// The TextView will expand to fit all content, no scrollbar.
    /// This mode is suitable for small content, such as a few lines of text, a label, etc.
    pub fn scrollable(mut self, scrollable: bool) -> Self {
        self.scrollable = scrollable;
        self
    }

    /// Set the exact plain text copied when Select All is active.
    ///
    /// This is useful when the rendered source contains markup or multiple
    /// virtualized code blocks but represents one logical document.
    pub fn select_all_text(mut self, text: impl Into<SharedString>) -> Self {
        self.select_all_text = Some(text.into());
        self
    }

    /// Returns true when the currently painted document was parsed from this
    /// view's exact source text.
    pub fn is_current(&self, cx: &App) -> bool {
        self.state.read(cx).parsed_text.as_ref() == Some(&self.raw)
    }

    fn on_action_copy(state: &Entity<TextViewState>, cx: &mut App) {
        let Some(selected_text) = state.read(cx).selection_text() else {
            return;
        };

        let selected_text = if state.read(cx).is_all_selected {
            selected_text
        } else {
            selected_text.trim().to_string()
        };
        cx.write_to_clipboard(ClipboardItem::new_string(selected_text));
    }

    /// Set custom block actions for code blocks.
    ///
    /// The closure receives the [`CodeBlock`],
    /// and returns an element to display.
    pub fn code_block_actions<F, E>(mut self, f: F) -> Self
    where
        F: Fn(&CodeBlock, &mut Window, &mut App) -> E + Send + Sync + 'static,
        E: IntoElement,
    {
        self.code_block_actions = Some(Arc::new(move |code_block, window, cx| {
            f(&code_block, window, cx).into_any_element()
        }));
        self
    }
}

impl IntoElement for TextView {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextView {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        if let Some(InitState::Initializing {
            type_,
            text,
            style,
            highlight_theme,
        }) = self.init_state.take()
        {
            let style = *style;
            let highlight_theme = highlight_theme.clone();
            let code_block_actions = self.code_block_actions.clone();
            let (tx, rx) = smol::channel::unbounded::<Update>();
            let (tx_result, rx_result) = smol::channel::unbounded::<ParsedUpdate>();
            let parsed_result = parse_content(
                type_,
                &text,
                style.clone(),
                &highlight_theme,
                &code_block_actions,
            );

            self.state.update(cx, {
                let tx = tx.clone();
                |state, _| {
                    state.tx = Some(tx);
                    state.install_parsed_update(ParsedUpdate {
                        text: text.clone(),
                        result: parsed_result,
                    });
                }
            });

            cx.spawn({
                let state = self.state.downgrade();
                async move |cx| {
                    while let Ok(parsed_update) = rx_result.recv().await {
                        if let Some(state) = state.upgrade() {
                            _ = state.update(cx, |state, cx| {
                                if state.scroll_handle.is_dragging() {
                                    state.pending_parsed_update = Some(parsed_update);
                                } else {
                                    state.install_parsed_update(parsed_update);
                                }
                                if let Some(parent_entity) = state.parent_entity {
                                    let app = &mut **cx;
                                    app.notify(parent_entity);
                                }
                            });
                        } else {
                            // state released, stopping processing
                            break;
                        }
                    }
                }
            })
            .detach();

            cx.background_spawn(UpdateFuture::new(
                type_,
                style,
                text,
                highlight_theme,
                rx,
                tx_result,
                Duration::from_millis(200),
                code_block_actions,
            ))
            .detach();

            self.init_state = Some(InitState::Initialized { tx });
        }

        self.state.update(cx, |state, _| {
            state.scroll_handle.set_line_height(window.line_height());
            state.apply_pending_parsed_update();
        });
        let list_state = &self.state.read(cx).list_state;
        let scroll_handle = &self.state.read(cx).scroll_handle;

        let focus_handle = self
            .state
            .read(cx)
            .focus_handle
            .as_ref()
            .expect("focus_handle should init by TextViewState::new");

        let mut el = div()
            .key_context(CONTEXT)
            .track_focus(focus_handle)
            .size_full()
            .relative()
            .on_action({
                let state = self.state.clone();
                move |_: &input::Copy, _, cx| {
                    Self::on_action_copy(&state, cx);
                }
            })
            .on_action({
                let state = self.state.clone();
                let entity_id = window.current_view();
                move |_: &input::SelectAll, _, cx| {
                    state.update(cx, |state, _| state.select_all());
                    cx.notify(entity_id);
                }
            })
            .child(TextViewElement {
                list_state: if self.scrollable {
                    Some(list_state.clone())
                } else {
                    None
                },
                state: self.state.clone(),
            })
            .refine_style(&self.style)
            .vertical_scrollbar(scroll_handle)
            .into_any_element();
        let layout_id = el.request_layout(window, cx);
        (layout_id, el)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        request_layout.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let entity_id = window.current_view();
        let is_selectable = self.selectable;

        self.state.update(cx, |state, _| {
            state.parent_entity = Some(entity_id);
            state.update_bounds(bounds);
            state.is_selectable = is_selectable;
            state.select_all_text = self
                .select_all_text
                .clone()
                .or_else(|| Some(self.raw.clone()));
        });

        GlobalState::global_mut(cx)
            .text_view_state_stack
            .push(self.state.clone());
        request_layout.paint(window, cx);
        GlobalState::global_mut(cx).text_view_state_stack.pop();

        self.state.update(cx, |state, _| {
            if state.selection_needs_layout {
                state.finish_selection_layout();
            }
        });

        if self.selectable {
            let has_selection = self.state.read(cx).has_selection();

            window.on_mouse_event({
                let state = self.state.clone();
                let focus_handle = self
                    .state
                    .read(cx)
                    .focus_handle
                    .as_ref()
                    .expect("focus handle initialized")
                    .clone();
                move |event: &MouseDownEvent, phase, window, cx| {
                    if !bounds.contains(&event.position) || !phase.bubble() {
                        return;
                    }

                    focus_handle.focus(window);
                    state.update(cx, |state, _| {
                        state.start_selection(event.position);
                    });
                    cx.notify(entity_id);
                }
            });

            // Keep these handlers installed even before selection starts. A
            // fast drag can deliver down, move, and up within one frame, so
            // waiting for the post-down paint would lose its final endpoint.
            window.on_mouse_event({
                let state = self.state.clone();
                move |event: &MouseMoveEvent, phase, _, cx| {
                    if !phase.bubble() || !state.read(cx).is_selecting {
                        return;
                    }

                    state.update(cx, |state, _| {
                        state.update_selection(event.position);
                    });
                    cx.notify(entity_id);
                }
            });

            window.on_mouse_event({
                let state = self.state.clone();
                move |_: &MouseUpEvent, phase, _, cx| {
                    if !phase.bubble() || !state.read(cx).is_selecting {
                        return;
                    }

                    state.update(cx, |state, _| {
                        state.end_selection();
                    });
                    cx.notify(entity_id);
                }
            });

            if has_selection {
                // down outside to clear selection
                window.on_mouse_event({
                    let state = self.state.clone();
                    move |event: &MouseDownEvent, _, _, cx| {
                        if bounds.contains(&event.position) {
                            return;
                        }

                        state.update(cx, |state, _| {
                            state.clear_selection();
                        });
                        cx.notify(entity_id);
                    }
                });
            }
        }
    }
}

fn parse_content(
    type_: TextViewType,
    text: &str,
    style: TextViewStyle,
    highlight_theme: &HighlightTheme,
    code_block_actions: &Option<Arc<CodeBlockActionsFn>>,
) -> Result<ParsedContent, SharedString> {
    let mut node_cx = NodeContext {
        style: style.clone(),
        code_block_actions: code_block_actions.clone(),
        ..NodeContext::default()
    };

    let res = match type_ {
        TextViewType::Markdown => {
            super::format::markdown::parse(text, &style, &mut node_cx, highlight_theme)
        }
        TextViewType::Html => super::format::html::parse(text, &mut node_cx),
    };
    res.map(move |root_node| ParsedContent { root_node, node_cx })
}

fn selection_bounds(
    start: Option<Point<Pixels>>,
    end: Option<Point<Pixels>>,
    bounds: Bounds<Pixels>,
) -> Bounds<Pixels> {
    if let (Some(start), Some(end)) = (start, end) {
        let start = start + bounds.origin;
        let end = end + bounds.origin;

        let origin = Point {
            x: start.x.min(end.x),
            y: start.y.min(end.y),
        };
        let size = Size {
            width: (start.x - end.x).abs(),
            height: (start.y - end.y).abs(),
        };

        return Bounds { origin, size };
    }

    Bounds::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, point, px, size};

    fn text_view_state_for_test() -> TextViewState {
        let list_state = ListState::new(0, gpui::ListAlignment::Top, px(1000.));
        let scroll_handle = TextViewScrollHandle::new(list_state.clone());
        TextViewState {
            parent_entity: None,
            tx: None,
            parsed_result: None,
            parsed_text: None,
            focus_handle: None,
            bounds: Bounds::default(),
            selection_positions: (None, None),
            is_selecting: false,
            selection_needs_layout: false,
            is_all_selected: false,
            is_selectable: true,
            select_all_text: None,
            list_state,
            scroll_handle,
            pending_parsed_update: None,
        }
    }

    #[test]
    fn estimated_scrollbar_maps_the_full_virtual_document() {
        let list_state = ListState::new(3, gpui::ListAlignment::Top, px(1000.));
        let handle = TextViewScrollHandle::new(list_state.clone());
        handle.set_line_height(px(20.));
        handle.set_row_weights(vec![10, 10, 10]);

        handle.set_offset(point(px(0.), px(-400.)));

        assert_eq!(list_state.logical_scroll_top().item_ix, 2);
        assert_eq!(handle.offset().y, px(-400.));
        assert_eq!(handle.content_size().height, px(600.));
    }

    #[test]
    fn parsed_updates_stay_deferred_while_the_scrollbar_is_dragged() {
        let mut state = text_view_state_for_test();
        state.scroll_handle.start_drag();
        state.pending_parsed_update = Some(ParsedUpdate {
            text: SharedString::from("new"),
            result: Err(SharedString::from("deferred")),
        });

        state.apply_pending_parsed_update();
        assert!(state.pending_parsed_update.is_some());

        state.scroll_handle.end_drag();
        state.apply_pending_parsed_update();
        assert!(state.pending_parsed_update.is_none());
        assert_eq!(
            state.parsed_text.as_ref().map(|text| text.as_ref()),
            Some("new")
        );
    }

    #[test]
    fn select_all_uses_the_exact_logical_document() {
        let mut state = text_view_state_for_test();
        state.select_all_text = Some(SharedString::from("first\nsecond\n"));

        state.select_all();

        assert!(state.has_selection());
        assert!(state.is_all_selected());
        assert_eq!(state.selection_text().as_deref(), Some("first\nsecond\n"));

        state.clear_selection();
        assert!(!state.has_selection());
    }

    #[test]
    fn finalized_mouse_selection_keeps_its_document_endpoints() {
        let mut state = text_view_state_for_test();
        state.bounds = Bounds {
            origin: point(px(100.), px(200.)),
            size: size(px(500.), px(400.)),
        };
        state.start_selection(point(px(110.), px(220.)));
        state.update_selection(point(px(210.), px(320.)));
        state.end_selection();

        assert!(state.has_selection());
        assert!(!state.is_selecting());
        assert!(state.selection_needs_layout());
        assert_eq!(
            state.selection_bounds(),
            Bounds::from_corners(point(px(110.), px(220.)), point(px(210.), px(320.)))
        );

        state.finish_selection_layout();
        assert!(!state.selection_needs_layout());
        assert!(state.has_selection());
    }

    #[test]
    fn test_text_view_state_selection_bounds() {
        assert_eq!(
            selection_bounds(None, None, Default::default()),
            Bounds::default()
        );
        assert_eq!(
            selection_bounds(None, Some(point(px(10.), px(20.))), Default::default()),
            Bounds::default()
        );
        assert_eq!(
            selection_bounds(Some(point(px(10.), px(20.))), None, Default::default()),
            Bounds::default()
        );

        // 10,10 start
        //   |------|
        //   |      |
        //   |------|
        //         50,50
        assert_eq!(
            selection_bounds(
                Some(point(px(10.), px(10.))),
                Some(point(px(50.), px(50.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        // 10,10
        //   |------|
        //   |      |
        //   |------|
        //         50,50 start
        assert_eq!(
            selection_bounds(
                Some(point(px(50.), px(50.))),
                Some(point(px(10.), px(10.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        //        50,10 start
        //   |------|
        //   |      |
        //   |------|
        // 10,50
        assert_eq!(
            selection_bounds(
                Some(point(px(50.), px(10.))),
                Some(point(px(10.), px(50.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
        //        50,10
        //   |------|
        //   |      |
        //   |------|
        // 10,50 start
        assert_eq!(
            selection_bounds(
                Some(point(px(10.), px(50.))),
                Some(point(px(50.), px(10.))),
                Default::default()
            ),
            Bounds {
                origin: point(px(10.), px(10.)),
                size: size(px(40.), px(40.))
            }
        );
    }
}
