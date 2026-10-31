//! UI-local management content, request lifetime, index selection and drawing.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagementMode {
    General,
    Received,
    Status,
    Index,
    Attachments,
}
pub(super) struct ManagementPage {
    pub(super) source_request_id: u64,
    pub(super) observed_at: String,
    pub(super) stale: Option<String>,
    pub(super) pointer: String,
    pub(super) text: String,
    pub(super) next_cursor: Option<String>,
}
pub(super) struct StructuredManagementState {
    pub(super) detail_request_id: u64,
    pub(super) preserve_detail_scroll: bool,
    pub(super) detail_error: Option<String>,
    pub(super) request_id: u64,
    pub(super) fields: Vec<PresentationField>,
    pub(super) selected: usize,
    pub(super) omitted: bool,
    pub(super) detail: Option<ManagementPage>,
    pub(super) pending_pointer: Option<String>,
}

struct ManagementFrame {
    view: ManagementView,
    structured: Option<StructuredManagementState>,
}
#[derive(Default)]
pub(super) enum IndexPreviewState {
    #[default]
    Idle,
    Loading(IndexArgs),
    Ready(IndexArgs, String),
    Failed(IndexArgs, String),
    Hint(String),
}
impl IndexPreviewState {
    fn ready(&self, args: &IndexArgs) -> Option<&str> {
        match self {
            Self::Ready(selected, text) if selected == args => Some(text),
            _ => None,
        }
    }
}
pub struct ManagementPopup {
    pub(super) inspection: Option<inspection::State>,
    inspection_detail: Option<UiAction>,
    paint: std::cell::Cell<Option<popup::PaintBounds>>,
    reader_cache: std::cell::RefCell<popup::ReaderCache>,
    reader_revision: u64,
    frame: Option<ManagementFrame>,
    pub(super) owner: Option<ManagementContext>,
    pub(super) mode: ManagementMode,
    pub(super) epoch: u64,
    pub(super) scope: Option<IndexArgs>,
    pub(super) preview: IndexPreviewState,
    pub(super) pending: Option<Command>,
}
impl Default for ManagementPopup {
    fn default() -> Self {
        Self {
            inspection: None,
            inspection_detail: None,
            paint: Default::default(),
            reader_cache: Default::default(),
            reader_revision: 0,
            frame: None,
            owner: None,
            mode: ManagementMode::General,
            epoch: 0,
            scope: None,
            preview: IndexPreviewState::Idle,
            pending: None,
        }
    }
}
pub(super) enum Outcome {
    Original(String),
    Actions(Vec<UiAction>),
    Close,
    Operations,
    Confirm { command: Command, detail: String },
}
impl ManagementPopup {
    pub(super) fn start_inspection(&mut self, target: InspectionTarget, title: String) {
        // Pending cost intents belong to their exact Index selection. Admitted
        // operations are tracked elsewhere and are never cancelled here.
        let same_index_scope = matches!(&target,InspectionTarget::Index(args) if self.mode==ManagementMode::Index && self.scope.as_ref().map_or_else(||args==&IndexArgs::default(),|current|current==args));
        if !same_index_scope {
            self.pending = None;
            self.reset_preview();
            self.scope = None;
        }
        self.open_structured(title, 0, vec![], false, false);
        if let InspectionTarget::Index(args) = &target {
            self.mode = ManagementMode::Index;
            self.scope = Some(args.clone());
        } else if matches!(target, InspectionTarget::Attachments { .. }) {
            self.mode = ManagementMode::Attachments;
        } else {
            self.mode = ManagementMode::General;
        }
        let desired = match &target {
            InspectionTarget::Attachments {
                thread_id,
                attachment_id: Some(id),
            } => Some(InspectionFieldAnchor {
                pointer: String::new(),
                entity: Some(("attachment_selection".into(), format!("{thread_id}/{id}"))),
                relative_pointer: "/attachment_id".into(),
                stable: true,
            }),
            _ => None,
        };
        let mut state = inspection::State::new(target);
        state.unavailable_selection = desired;
        self.inspection = Some(state);
        self.inspection_detail = None;
    }
    pub(super) fn patch_inspection(
        &mut self,
        request_id: u64,
        snapshot: InspectionSnapshot,
    ) -> bool {
        let Some(inspection) = self.inspection.as_ref() else {
            return false;
        };
        if (inspection.has_snapshot
            && self
                .structured()
                .is_some_and(|state| request_id <= state.request_id))
            || inspection.target != snapshot.target
            || snapshot.metadata_sequence < inspection.metadata_sequence
            || inspection
                .usage_sequence
                .is_some_and(|old| snapshot.usage_sequence.is_none_or(|new| new < old))
            || snapshot.fields.len() > 256
            || snapshot.anchors.len() > 256
        {
            return false;
        }
        let selected = if inspection.reset_selection {
            None
        } else {
            inspection.unavailable_selection.clone().or_else(|| {
                self.structured()
                    .and_then(|state| state.fields.get(state.selected))
                    .and_then(|field| {
                        inspection
                            .anchors
                            .iter()
                            .find(|anchor| anchor.pointer == field.key)
                    })
                    .cloned()
            })
        };
        let selected_index = selected
            .as_ref()
            .and_then(|old| snapshot.anchors.iter().find(|new| old.same_source(new)))
            .and_then(|anchor| {
                snapshot
                    .fields
                    .iter()
                    .position(|field| field.key == anchor.pointer)
            });
        let detail_anchor = inspection.detail_anchor.clone();
        let mapped = detail_anchor
            .as_ref()
            .and_then(|old| snapshot.anchors.iter().find(|new| old.same_source(new)))
            .map(|anchor| anchor.pointer.clone());
        let Some(frame) = self.frame.as_mut() else {
            return false;
        };
        let Some(state) = frame.structured.as_mut() else {
            return false;
        };
        let detail_consumer = inspection.detail_requested
            || state.detail.is_some()
            || state.pending_pointer.is_some()
            || self.inspection_detail.is_some();
        state.selected = selected_index
            .unwrap_or_else(|| state.selected.min(snapshot.fields.len().saturating_sub(1)));
        state.fields = snapshot.fields;
        state.omitted = snapshot.omitted;
        state.request_id = request_id;
        state.detail_request_id = state.detail_request_id.wrapping_add(1);
        state.pending_pointer = None;
        self.inspection_detail = None;
        let inspection = self.inspection.as_mut().unwrap();
        inspection.reset_selection = false;
        inspection.selection_unavailable = selected.is_some() && selected_index.is_none();
        inspection.unavailable_selection = inspection
            .selection_unavailable
            .then(|| selected.clone())
            .flatten();
        inspection.metadata_sequence = snapshot.metadata_sequence;
        inspection.usage_sequence = snapshot.usage_sequence;
        inspection.metadata_observed_at = snapshot.metadata_observed_at;
        inspection.usage_observed_at = snapshot.usage_observed_at;
        inspection.anchors = snapshot.anchors;
        inspection.has_snapshot = true;
        if inspection
            .metadata_error
            .as_ref()
            .is_some_and(|(seq, _)| *seq < snapshot.metadata_sequence)
        {
            inspection.metadata_error = None;
        }
        if inspection
            .usage_error
            .as_ref()
            .is_some_and(|(seq, _)| snapshot.usage_sequence.is_some_and(|new| new > *seq))
        {
            inspection.usage_error = None;
        }
        inspection.detail_pointer = mapped.clone();
        if let Some(detail) = state.detail.as_mut() {
            detail.stale = Some(
                if mapped.is_some() {
                    "新しい保存状態の詳細を取得中"
                } else {
                    "参照元が消失・一意に確認できません"
                }
                .into(),
            );
        }
        if detail_consumer && let Some(pointer) = mapped {
            state.pending_pointer = Some(pointer.clone());
            state.preserve_detail_scroll = state.detail.is_some();
            self.inspection_detail = Some(UiAction::LoadManagementDetail {
                request_id,
                detail_request_id: state.detail_request_id,
                pointer,
                cursor: None,
            });
        }
        self.paint.set(None);
        true
    }
    pub(super) fn fail_inspection(
        &mut self,
        target: InspectionTarget,
        part: InspectionPart,
        sequence: u64,
        observed_at: String,
        error: String,
    ) -> bool {
        let Some(state) = self
            .inspection
            .as_mut()
            .filter(|state| state.target == target)
        else {
            return false;
        };
        let prefix = error.chars().take(512).collect::<String>();
        let message = format!(
            "{} · {}",
            observed_at,
            crate::presentation::sanitize_plain(&prefix, 0).0
        );
        match part {
            InspectionPart::Metadata => {
                if sequence < state.metadata_sequence
                    || state
                        .metadata_error
                        .as_ref()
                        .is_some_and(|(old, _)| sequence < *old)
                {
                    return false;
                }
                state.metadata_error = Some((sequence, message));
            }
            InspectionPart::Usage => {
                if state.usage_sequence.is_some_and(|old| sequence < old)
                    || state
                        .usage_error
                        .as_ref()
                        .is_some_and(|(old, _)| sequence < *old)
                {
                    return false;
                }
                state.usage_error = Some((sequence, message));
            }
        }
        if let Some(page) = self
            .frame
            .as_mut()
            .and_then(|frame| frame.structured.as_mut())
            .and_then(|state| state.detail.as_mut())
        {
            page.stale = Some("更新に失敗・以前の取得内容".into());
        }
        true
    }
    pub(super) fn fail_detail(
        &mut self,
        request_id: u64,
        detail_request_id: u64,
        pointer: &str,
        error: &str,
    ) -> bool {
        let Some(state) = self.structured_mut() else {
            return false;
        };
        if state.request_id != request_id
            || state.detail_request_id != detail_request_id
            || state.pending_pointer.as_deref() != Some(pointer)
        {
            return false;
        }
        let prefix = error.chars().take(512).collect::<String>();
        let reason = crate::presentation::sanitize_plain(&prefix, 0).0;
        state.pending_pointer = None;
        state.detail_error = Some(reason.clone());
        if let Some(page) = &mut state.detail {
            page.stale = Some(format!("詳細の更新に失敗: {reason}"));
        }
        true
    }
    pub(super) fn take_inspection_detail(&mut self) -> Option<UiAction> {
        self.inspection_detail.take()
    }
    pub(super) fn detail_rejected(&mut self, reason: &str) {
        if let Some(state) = self.structured_mut() {
            state.pending_pointer = None;
            if let Some(page) = &mut state.detail {
                page.stale = Some(reason.into());
            }
        }
        self.inspection_detail = None;
    }
    pub(super) fn paint_bounds(&self) -> Option<popup::PaintBounds> {
        self.paint.get()
    }
    pub(super) fn wheel(&mut self, delta: isize) -> bool {
        if let Some(state) = self.structured_mut()
            && state.detail.is_none()
        {
            let old = state.selected;
            state.selected = popup::move_selection(old, delta, state.fields.len());
            let changed = old != state.selected;
            if changed && let Some(inspection) = self.inspection.as_mut() {
                inspection.selection_unavailable = false;
                inspection.unavailable_selection = None;
            }
            return changed;
        }
        let maximum = self.reader_cache.borrow().bounds().maximum;
        if let Some(view) = self.view_mut() {
            let old = view.scroll;
            view.scroll = popup::move_reader(old, delta, maximum);
            return old != view.scroll;
        }
        false
    }
    fn reset_reader(&mut self) {
        self.paint.set(None);
        self.reader_cache.get_mut().clear();
        self.reader_revision = self.reader_revision.wrapping_add(1);
    }
    pub fn is_some(&self) -> bool {
        self.frame.is_some()
    }
    pub fn is_none(&self) -> bool {
        self.frame.is_none()
    }
    pub fn as_ref(&self) -> Option<&ManagementView> {
        self.frame.as_ref().map(|frame| &frame.view)
    }
    pub(super) fn view_mut(&mut self) -> Option<&mut ManagementView> {
        self.frame.as_mut().map(|frame| &mut frame.view)
    }
    pub(super) fn structured(&self) -> Option<&StructuredManagementState> {
        self.frame.as_ref()?.structured.as_ref()
    }
    pub(super) fn structured_mut(&mut self) -> Option<&mut StructuredManagementState> {
        self.frame.as_mut()?.structured.as_mut()
    }
    pub(super) fn open_text(&mut self, view: ManagementView) {
        self.inspection = None;
        self.inspection_detail = None;
        self.reset_reader();
        self.frame = Some(ManagementFrame {
            view,
            structured: None,
        });
    }
    pub(super) fn open_structured(
        &mut self,
        title: String,
        request_id: u64,
        fields: Vec<PresentationField>,
        omitted: bool,
        work_status: bool,
    ) {
        self.inspection = None;
        self.inspection_detail = None;
        self.reset_reader();
        if work_status {
            self.mode = ManagementMode::Status;
        } else if self.mode == ManagementMode::Status {
            self.mode = ManagementMode::General;
        }
        self.frame = Some(ManagementFrame {
            view: ManagementView {
                title,
                text: String::new(),
                scroll: 0,
            },
            structured: Some(StructuredManagementState {
                detail_request_id: 0,
                preserve_detail_scroll: false,
                detail_error: None,
                request_id,
                fields,
                selected: 0,
                omitted,
                detail: None,
                pending_pointer: None,
            }),
        });
    }
    pub(super) fn accept_detail(
        &mut self,
        request_id: u64,
        detail_request_id: u64,
        pointer: String,
        text: String,
        next_cursor: Option<String>,
    ) {
        if let Some(frame) = &mut self.frame
            && let Some(state) = &mut frame.structured
            && state.request_id == request_id
            && state.detail_request_id == detail_request_id
            && state.pending_pointer.as_deref() == Some(&pointer)
        {
            self.paint.set(None);
            self.reader_cache.get_mut().clear();
            self.reader_revision = self.reader_revision.wrapping_add(1);
            state.detail = Some(ManagementPage {
                source_request_id: request_id,
                observed_at: self
                    .inspection
                    .as_ref()
                    .map(|state| state.metadata_observed_at.clone())
                    .unwrap_or_default(),
                stale: None,
                pointer,
                text,
                next_cursor,
            });
            state.pending_pointer = None;
            state.detail_error = None;
            if !state.preserve_detail_scroll {
                frame.view.scroll = 0;
            }
            state.preserve_detail_scroll = false;
        }
    }
    pub(super) fn append_text(&mut self, title: String, text: String) {
        let mut body = self
            .frame
            .take()
            .map(|frame| frame.view.text)
            .unwrap_or_default();
        body.push_str(&format!("\n\n{title}\n{text}"));
        if body.len() > 65536 {
            let start = body.ceil_char_boundary(body.len() - 65536);
            body.drain(..start);
        }
        self.open_text(ManagementView {
            title,
            text: body,
            scroll: 0,
        });
    }
    pub(super) fn close(&mut self) {
        self.reset_reader();
        self.epoch = self.epoch.wrapping_add(1);
        self.frame = None;
        self.inspection = None;
        self.inspection_detail = None;
        self.paint.set(None);
        self.reader_cache.get_mut().clear();
        self.owner = None;
        self.mode = ManagementMode::General;
        self.scope = None;
        self.reset_preview();
        self.pending = None;
    }
    pub(super) fn accepts(
        &self,
        context: &ManagementContext,
        draft: &str,
        thread: Option<&str>,
    ) -> bool {
        self.owner.as_ref() == Some(context)
            && context.epoch == self.epoch
            && context
                .owner_thread
                .as_deref()
                .map_or(context.owner_draft == draft, |owner| Some(owner) == thread)
    }
    pub(super) fn reset_preview(&mut self) {
        self.preview = IndexPreviewState::Idle;
    }
    fn matches_preview(&self, args: &IndexArgs) -> bool {
        self.mode == ManagementMode::Index
            && self
                .scope
                .as_ref()
                .map_or_else(|| args == &IndexArgs::default(), |scope| scope == args)
    }
    pub(super) fn begin_preview(&mut self, args: IndexArgs) {
        if self.matches_preview(&args) {
            self.preview = IndexPreviewState::Loading(args);
        }
    }
    pub(super) fn accept_preview(&mut self, args: IndexArgs, text: String) -> bool {
        if !self.matches_preview(&args) {
            return false;
        }
        self.preview = IndexPreviewState::Ready(args, text);
        true
    }
    pub(super) fn fail_preview(&mut self, args: IndexArgs, message: String) {
        if self.matches_preview(&args) {
            self.pending = None;
            self.preview = IndexPreviewState::Failed(args, message);
        }
    }
    fn preview_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.mode != ManagementMode::Index
            || self
                .structured()
                .is_some_and(|value| value.detail.is_some())
        {
            return vec![];
        }
        let (title, text) = match &self.preview {
            IndexPreviewState::Idle => return vec![],
            IndexPreviewState::Loading(args) => (
                if explicit_index_targets(args) {
                    "費用preview · 取得中"
                } else {
                    "再試行preview · 取得中"
                },
                "件数と文字数を確認中 · API未送信",
            ),
            IndexPreviewState::Ready(args, text) => (
                if explicit_index_targets(args) {
                    "費用preview · 取得済み"
                } else {
                    "再試行preview · 取得済み"
                },
                text.as_str(),
            ),
            IndexPreviewState::Failed(args, text) => (
                if explicit_index_targets(args) {
                    "費用preview · 取得失敗"
                } else {
                    "再試行preview · 取得失敗"
                },
                text.as_str(),
            ),
            IndexPreviewState::Hint(text) => ("索引操作 · 未受付", text.as_str()),
        };
        let mut lines =
            vec![Line::from(title.to_owned()).style(Style::default().fg(theme::EMPHASIS))];
        let safe: String = crate::presentation::sanitize_plain(text, 0)
            .0
            .chars()
            .take(4096)
            .collect();
        lines.extend(safe.lines().map(|line| Line::from(line.to_owned())));
        presentation::hanging_wrap(lines, width.max(1))
    }
    fn preview_height(&self, area: Rect) -> u16 {
        let count = self.preview_lines(area.width).len();
        if count == 0 || area.height == 0 {
            return 0;
        }
        u16::try_from(count)
            .unwrap_or(u16::MAX)
            .min(area.height.saturating_sub(3).max(1))
            .min(12)
    }
    fn draw_preview(&self, frame: &mut Frame<'_>, area: Rect) -> Rect {
        let height = self.preview_height(area);
        if height == 0 {
            return area;
        }
        let mut lines = self.preview_lines(area.width);
        if lines.len() > usize::from(height) {
            lines.truncate(usize::from(height));
            if let Some(last) = lines.last_mut() {
                last.spans.push(Span::raw(" …"));
            }
        }
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(area.x, area.y, area.width, height),
        );
        Rect::new(
            area.x,
            area.y.saturating_add(height),
            area.width,
            area.height.saturating_sub(height),
        )
    }
    pub(super) fn index_label(&self) -> String {
        let target = match &self.scope {
            Some(args) if args.scope == Some(IndexScope::All) => {
                "全会話 / 保持された出典（all）".into()
            }
            Some(args) if !args.thread_ids.is_empty() => {
                format!("会話ID: {}", args.thread_ids.join(", "))
            }
            _ => "対象未選択。/index all または /index threads ID...".into(),
        };
        format!(
            "space={} / {}",
            self.scope
                .as_ref()
                .and_then(|args| args.space_id.as_deref())
                .unwrap_or("active"),
            target
        )
    }
    fn has_next_inspection_window(&self) -> bool {
        self.inspection
            .as_ref()
            .is_some_and(|inspection| matches!(inspection.target, InspectionTarget::Index(_)))
            && self.structured().is_some_and(|state| {
                state.fields.iter().any(|field| {
                    field.key == "/management/coverage_window/next_cursor"
                        && !field.value.is_empty()
                        && field.value != "null"
                })
            })
    }
    fn hints(&self) -> &'static str {
        let list_mode = self
            .structured()
            .is_some_and(|value| value.detail.is_none());
        match self.mode {
            ManagementMode::Index => {
                if self.has_next_inspection_window() {
                    "Esc 戻る · ↑↓/PgUp/PgDn 閲覧 · Ctrl+R 更新 · N 次window\nS 状態 · P 費用preview · B backfill · T retry · R rebuild"
                } else {
                    "Esc 戻る · ↑↓/PgUp/PgDn 閲覧 · Ctrl+R 更新\nS 状態 · P 費用preview · B backfill · T retry · R rebuild"
                }
            }
            ManagementMode::Attachments if list_mode => {
                "Esc 戻る · Tab 選択 · Enter 詳細 · C コピー\nS 保存済み一覧 · R 再読込 · 選択解除は戻って /detach ID"
            }
            ManagementMode::Attachments => {
                "Esc 戻る · ↑↓/PgUp/PgDn 閲覧\nS 保存済み一覧 · R 再読込 · 解除は戻って /detach ID"
            }
            ManagementMode::Status if list_mode => {
                "Esc 戻る · ↑↓ 選択 · Enter 詳細 · R 更新 · N 次page\nO 参照 · S 出力原文 · C コピー · A 操作"
            }
            ManagementMode::General if list_mode => {
                "Esc 戻る · Tab/Shift+Tab 選択 · Enter 詳細 · C コピー\nO 参照 · S 出力原文 · A 操作 · PgUp/PgDn 閲覧"
            }
            ManagementMode::Received => {
                "Esc 戻る · ↑↓ 選択 · Enter 原文／ページ · C IDコピー\n/received ページ番号 で全受信記録を表示"
            }
            ManagementMode::General | ManagementMode::Status => {
                "Esc 戻る · ↑↓/PgUp/PgDn 閲覧\nW 明示waitの観測取消 · A 操作"
            }
        }
    }
    pub(super) fn page(&self, viewport: Rect, status: &str) -> u16 {
        let detailed = self
            .structured()
            .is_some_and(|value| value.detail.is_some());
        let context = if self.structured().is_some() || self.mode == ManagementMode::Index {
            "項目"
        } else {
            status
        };
        let footer = if detailed {
            MANAGEMENT_DETAIL_FOOTER
        } else {
            self.hints()
        };
        let body = popup::body_area(viewport, context, footer, None);
        body.height
            .saturating_sub(self.preview_height(body))
            .saturating_sub(1)
            .max(1)
    }
    pub(super) fn key(&mut self, key: KeyEvent, page: u16, thread_id: Option<String>) -> Outcome {
        let refresh = matches!(key.code, KeyCode::Char('r' | 'R'))
            && (key.modifiers.contains(KeyModifiers::CONTROL)
                || (!matches!(
                    self.mode,
                    ManagementMode::Index | ManagementMode::Attachments
                ) && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)));
        if refresh && let (Some(inspection), Some(context)) = (&self.inspection, &self.owner) {
            return Outcome::Actions(vec![UiAction::ObserveInspection {
                context: context.clone(),
                target: inspection.target.clone(),
                visible: true,
                revision: 0,
            }]);
        }
        if key.code == KeyCode::Char('n')
            && key.modifiers.is_empty()
            && self.mode == ManagementMode::Index
            && self
                .structured()
                .is_some_and(|state| state.detail.is_none())
            && self.has_next_inspection_window()
        {
            let (Some(context), Some(inspection), Some(state)) = (
                self.owner.clone(),
                self.inspection.as_mut(),
                self.frame
                    .as_mut()
                    .and_then(|frame| frame.structured.as_mut()),
            ) else {
                return Outcome::Actions(vec![]);
            };
            let target = inspection.target.clone();
            let request_id = state.request_id;
            inspection.reset_selection = true;
            inspection.selection_unavailable = false;
            inspection.unavailable_selection = None;
            inspection.detail_anchor = None;
            inspection.detail_pointer = None;
            inspection.detail_requested = false;
            state.selected = 0;
            state.pending_pointer = None;
            state.detail_request_id = state.detail_request_id.wrapping_add(1);
            self.inspection_detail = None;
            self.paint.set(None);
            return Outcome::Actions(vec![UiAction::NextInspectionWindow {
                context,
                request_id,
                target,
            }]);
        }
        if self.mode == ManagementMode::Received
            && key.code == KeyCode::Enter
            && let Some(field) = self.structured().and_then(|s| s.fields.get(s.selected))
        {
            if field.key.starts_with("/received/source/") {
                return Outcome::Original(field.value.clone());
            }
            if let Some(page) = field
                .key
                .strip_prefix("/received/page/")
                .and_then(|p| p.parse::<u32>().ok())
            {
                return Outcome::Actions(vec![UiAction::LoadReceived {
                    thread_id: field.value.clone(),
                    page,
                }]);
            }
        }

        if key.code == KeyCode::Char('w') {
            return Outcome::Actions(vec![UiAction::CancelWait]);
        }
        if key.code == KeyCode::Char('a') {
            return Outcome::Operations;
        }
        if key.code == KeyCode::Esc && self.structured().is_none_or(|value| value.detail.is_none())
        {
            return Outcome::Close;
        }
        if self.mode == ManagementMode::Index
            && let KeyCode::Char(action @ ('b' | 't' | 'r')) = key.code
        {
            let args = self.scope.clone().unwrap_or_default();
            if action != 't' && !explicit_index_targets(&args) {
                self.pending = None;
                self.preview = IndexPreviewState::Hint(
                    "対象を明示してください。Esc後 /index all または /index thread".into(),
                );
                return Outcome::Actions(vec![]);
            }
            let preview = self.preview.ready(&args).map(str::to_owned);
            let Some(preview) = preview else {
                if action == 't' {
                    self.pending = Some(Command::IndexRetry(args.clone()));
                    self.begin_preview(args.clone());
                    return Outcome::Actions(vec![UiAction::IndexPreview(args)]);
                }
                self.preview = IndexPreviewState::Hint(
                    "現在の対象のpreviewが必要です。Pで取得してください。".into(),
                );
                return Outcome::Actions(vec![]);
            };
            let command = match action {
                'b' => Command::IndexBackfill(args.clone()),
                't' => Command::IndexRetry(args.clone()),
                _ => Command::IndexRebuild(args.clone()),
            };
            Outcome::Confirm {
                command,
                detail: format!(
                    "費用対象: {}\n{preview}\n外部embedding APIへ送信し費用が発生します。見積りは取得時点の対象数で、金額未見積です。",
                    if action == 't' && !explicit_index_targets(&args) {
                        "失敗した許可済み索引作業（全体）".into()
                    } else {
                        self.index_label()
                    }
                ),
            }
        } else {
            Outcome::Actions(self.navigate(key, page, thread_id))
        }
    }
    fn navigate(&mut self, key: KeyEvent, page: u16, thread_id: Option<String>) -> Vec<UiAction> {
        let has_detail = self
            .structured()
            .is_some_and(|state| state.detail.is_some());
        if let Some(inspection) = self.inspection.as_mut() {
            if has_detail && key.code == KeyCode::Esc {
                inspection.detail_requested = false;
                inspection.detail_anchor = None;
                inspection.detail_pointer = None;
            }
            if !has_detail
                && matches!(
                    key.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                        | KeyCode::Tab
                        | KeyCode::BackTab
                )
            {
                inspection.selection_unavailable = false;
                inspection.unavailable_selection = None;
            }
            if inspection.selection_unavailable
                && (matches!(key.code, KeyCode::Enter | KeyCode::Char('o' | 's'))
                    || (!has_detail && key.code == KeyCode::Char('c')))
            {
                return vec![];
            }
        }
        let starts_field_detail = !has_detail
            && (key.code == KeyCode::Enter
                || (key.code == KeyCode::Char('c')
                    && self
                        .structured()
                        .and_then(|state| state.fields.get(state.selected))
                        .is_some_and(|field| field.value_omitted)));
        if starts_field_detail {
            let anchor = self
                .structured()
                .and_then(|state| state.fields.get(state.selected))
                .and_then(|field| {
                    self.inspection.as_ref().and_then(|inspection| {
                        inspection
                            .anchors
                            .iter()
                            .find(|anchor| anchor.pointer == field.key)
                    })
                })
                .cloned();
            if let Some(inspection) = self.inspection.as_mut() {
                inspection.detail_requested = true;
                inspection.detail_pointer = anchor.as_ref().map(|anchor| anchor.pointer.clone());
                inspection.detail_anchor = anchor;
            }
        }
        let inspection_pointer = self
            .inspection
            .as_ref()
            .and_then(|state| state.detail_pointer.clone());
        let is_status = self.mode == ManagementMode::Status;
        let is_index = self.mode == ManagementMode::Index;
        let is_attachments = self.mode == ManagementMode::Attachments;
        if let Some(structured) = self.structured_mut() {
            if structured.detail.is_none() {
                let mode = match key.code {
                    KeyCode::Char('o') => Some(ManagementFollow::Inspect),
                    KeyCode::Char('s') if !is_index && !is_attachments => {
                        Some(ManagementFollow::Read)
                    }
                    KeyCode::Char('r') if is_status => Some(ManagementFollow::Refresh),
                    KeyCode::Char('n') if is_status => Some(ManagementFollow::Next),
                    _ => None,
                };
                if let Some(mode) = mode {
                    return vec![UiAction::FollowManagement {
                        request_id: structured.request_id,
                        pointer: structured
                            .fields
                            .get(structured.selected)
                            .map(|field| field.key.clone())
                            .unwrap_or_default(),
                        mode,
                    }];
                }
            }
            match key.code {
                KeyCode::Tab | KeyCode::Down if structured.detail.is_none() => {
                    structured.selected =
                        (structured.selected + 1).min(structured.fields.len().saturating_sub(1));
                    return vec![];
                }
                KeyCode::BackTab | KeyCode::Up if structured.detail.is_none() => {
                    structured.selected = structured.selected.saturating_sub(1);
                    return vec![];
                }
                KeyCode::PageDown if structured.detail.is_none() => {
                    structured.selected = structured
                        .selected
                        .saturating_add(usize::from(page))
                        .min(structured.fields.len().saturating_sub(1));
                    return vec![];
                }
                KeyCode::PageUp if structured.detail.is_none() => {
                    structured.selected = structured.selected.saturating_sub(usize::from(page));
                    return vec![];
                }
                KeyCode::Tab if structured.detail.is_none() => {
                    structured.selected =
                        (structured.selected + 1).min(structured.fields.len().saturating_sub(1));
                    return vec![];
                }
                KeyCode::BackTab if structured.detail.is_none() => {
                    structured.selected = structured.selected.saturating_sub(1);
                    return vec![];
                }
                KeyCode::Char('c') => {
                    if let Some(page) = &structured.detail {
                        return vec![UiAction::CopyText {
                            text: page.text.clone(),
                            scope: CopyScope::ManagementPage {
                                has_more: page.next_cursor.is_some(),
                            },
                        }];
                    }
                    if let Some(field) = structured.fields.get(structured.selected) {
                        if field.value_omitted {
                            structured.detail_request_id =
                                structured.detail_request_id.wrapping_add(1);
                            structured.preserve_detail_scroll = false;
                            structured.pending_pointer = Some(field.key.clone());
                            return vec![UiAction::LoadManagementDetail {
                                request_id: structured.request_id,
                                detail_request_id: structured.detail_request_id,
                                pointer: field.key.clone(),
                                cursor: None,
                            }];
                        }
                        return vec![UiAction::CopyText {
                            text: field.value.clone(),
                            scope: CopyScope::Value,
                        }];
                    }
                }
                KeyCode::Enter => {
                    if let Some(field) = structured.fields.get(structured.selected) {
                        structured.detail_request_id = structured.detail_request_id.wrapping_add(1);
                        structured.preserve_detail_scroll = false;
                        structured.pending_pointer = Some(field.key.clone());
                        return vec![UiAction::LoadManagementDetail {
                            request_id: structured.request_id,
                            detail_request_id: structured.detail_request_id,
                            pointer: field.key.clone(),
                            cursor: None,
                        }];
                    }
                }
                KeyCode::Char('n') => {
                    if let Some(page) = &structured.detail
                        && page.source_request_id == structured.request_id
                        && let Some(cursor) = &page.next_cursor
                    {
                        structured.detail_request_id = structured.detail_request_id.wrapping_add(1);
                        structured.preserve_detail_scroll = false;
                        structured.pending_pointer = Some(page.pointer.clone());
                        return vec![UiAction::LoadManagementDetail {
                            request_id: structured.request_id,
                            detail_request_id: structured.detail_request_id,
                            pointer: page.pointer.clone(),
                            cursor: Some(cursor.clone()),
                        }];
                    }
                }
                KeyCode::Char('p') => {
                    if let Some(page) = &structured.detail {
                        let pointer = inspection_pointer.clone().or_else(|| {
                            (page.source_request_id == structured.request_id)
                                .then(|| page.pointer.clone())
                        });
                        let Some(pointer) = pointer else {
                            return vec![];
                        };
                        structured.detail_request_id = structured.detail_request_id.wrapping_add(1);
                        structured.preserve_detail_scroll = false;
                        structured.pending_pointer = Some(pointer.clone());
                        return vec![UiAction::LoadManagementDetail {
                            request_id: structured.request_id,
                            detail_request_id: structured.detail_request_id,
                            pointer,
                            cursor: None,
                        }];
                    }
                }
                KeyCode::Esc if structured.detail.is_some() => {
                    structured.detail = None;
                    structured.pending_pointer = None;
                    structured.detail_request_id = structured.detail_request_id.wrapping_add(1);
                    return vec![];
                }
                _ => {}
            }
        }
        if !matches!(
            key.code,
            KeyCode::Esc | KeyCode::PageDown | KeyCode::Down | KeyCode::PageUp | KeyCode::Up
        ) && self.mode != management::ManagementMode::Index
        {
            if self.mode == management::ManagementMode::Attachments
                && let Some(thread_id) = self
                    .inspection
                    .as_ref()
                    .and_then(|inspection| match &inspection.target {
                        InspectionTarget::Attachments { thread_id, .. } => Some(thread_id.clone()),
                        _ => None,
                    })
                    .or_else(|| thread_id.clone())
            {
                return match key.code {
                    KeyCode::Char('s') => {
                        vec![UiAction::Manage(Command::AttachmentList(ThreadTarget {
                            thread_id: Some(thread_id),
                        }))]
                    }
                    KeyCode::Char('r') => vec![UiAction::ReloadAttachments { thread_id }],
                    _ => vec![],
                };
            }
            return vec![];
        }
        match key.code {
            KeyCode::PageDown | KeyCode::Down | KeyCode::PageUp | KeyCode::Up => {
                if let Some(view) = self.view_mut() {
                    view.scroll =
                        popup::reader_key(key.code, view.scroll, page).unwrap_or(view.scroll);
                }
                vec![]
            }
            KeyCode::Char('s') => {
                let args = self.scope.clone().unwrap_or_default();
                vec![UiAction::Manage(Command::IndexStatus(args))]
            }
            KeyCode::Char('p') => {
                let Some(args) = self.scope.clone().filter(explicit_index_targets) else {
                    self.pending = None;
                    self.preview = IndexPreviewState::Hint("対象を明示してください。Esc後 /index all または /index thread。失敗分の再試行はTで確認できます。".into());
                    return vec![];
                };
                self.begin_preview(args.clone());
                vec![UiAction::IndexPreview(args)]
            }
            _ => vec![],
        }
    }
    pub(super) fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, status: &str) {
        if let Some(content) = &self.frame {
            let view = &content.view;
            let hints = self.hints();
            let mut reader_scroll = None;
            if let Some(structured) = &content.structured {
                if let Some(page) = &structured.detail {
                    let title = format!("{} · 詳細", view.title);
                    let context = format!(
                        "参照 {} · 取得 {}{}",
                        page.pointer,
                        page.observed_at,
                        page.stale
                            .as_ref()
                            .map(|reason| format!(" · 旧内容: {reason}"))
                            .unwrap_or_default()
                    );
                    let body = popup::panel_recorded(
                        frame,
                        area,
                        &title,
                        &context,
                        MANAGEMENT_DETAIL_FOOTER,
                        &self.paint,
                    );
                    reader_scroll = Some(self.reader_cache.borrow_mut().render(
                        frame,
                        body,
                        self.reader_revision,
                        view.scroll,
                        || {
                            crate::presentation::sanitize_plain(&page.text, 0)
                                .0
                                .lines()
                                .map(|line| Line::from(line.to_owned()))
                                .collect::<Vec<_>>()
                        },
                    ));
                } else {
                    let context = format!(
                        "{} / {}項目{} · {}",
                        if structured.fields.is_empty() {
                            0
                        } else {
                            structured.selected + 1
                        },
                        structured.fields.len(),
                        if structured.omitted {
                            " · すべての詳細を見る"
                        } else {
                            ""
                        },
                        match self.mode {
                            ManagementMode::Index => self.index_label(),
                            _ => status.to_owned(),
                        }
                    );
                    let context = if let Some(inspection) = &self.inspection {
                        format!("{} · {context}", inspection.label())
                    } else {
                        context
                    };
                    let context = if let Some(error) = &structured.detail_error {
                        format!("詳細未更新: {error} · {context}")
                    } else {
                        context
                    };
                    let body = popup::panel_recorded(
                        frame,
                        area,
                        &view.title,
                        &context,
                        hints,
                        &self.paint,
                    );
                    let body = self.draw_preview(frame, body);
                    let mut items = Vec::new();
                    let mut selected_row = 0;
                    let mut previous_group = "";
                    for (index, field) in structured.fields.iter().enumerate() {
                        let (group, label) = field
                            .label
                            .split_once(" › ")
                            .unwrap_or(("", field.label.as_str()));
                        if group != previous_group && !group.is_empty() {
                            items.push(popup::Item::new(
                                Line::from(format!("── {group} ──"))
                                    .style(Style::default().fg(theme::GROUP_HEADING)),
                            ));
                        }
                        previous_group = group;
                        if index == structured.selected {
                            selected_row = items.len();
                        }
                        let value = crate::presentation::sanitize_plain(field.displayed(), 0).0;
                        let label = crate::presentation::sanitize_plain(label, 0)
                            .0
                            .replace('\n', " ");
                        let mut value_lines = value.lines();
                        let first_value = value_lines.next().unwrap_or("").to_owned();
                        let mut lines = vec![Line::from(vec![
                            Span::styled(
                                format!("{label}: "),
                                Style::default().fg(theme::EMPHASIS),
                            ),
                            Span::raw(first_value),
                        ])];
                        lines =
                            presentation::hanging_wrap(lines, body.width.saturating_sub(3).max(1));
                        items.push(popup::Item::new(lines));
                    }
                    if items.is_empty() {
                        items.push(popup::Item::new("表示する項目はありません"));
                    }
                    popup::list(frame, body, items, selected_row);
                }
            } else {
                let context = match self.mode {
                    ManagementMode::Index => format!("対象 {} · {status}", self.index_label()),
                    _ => status.to_owned(),
                };
                let body =
                    popup::panel_recorded(frame, area, &view.title, &context, hints, &self.paint);
                let body = self.draw_preview(frame, body);
                reader_scroll = Some(self.reader_cache.borrow_mut().render(
                    frame,
                    body,
                    self.reader_revision,
                    view.scroll,
                    || {
                        crate::presentation::sanitize_plain(&view.text, 0)
                            .0
                            .lines()
                            .map(|line| Line::from(line.to_owned()))
                            .collect::<Vec<_>>()
                    },
                ));
            }
            if let Some(scroll) = reader_scroll {
                self.view_mut().unwrap().scroll = scroll;
            }
        }
    }
}
pub(super) fn explicit_index_targets(args: &IndexArgs) -> bool {
    args.scope.is_some() || !args.thread_ids.is_empty() || !args.source_ids.is_empty()
}
