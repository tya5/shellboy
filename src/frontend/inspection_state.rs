//! One visible saved inspection on the existing frontend read admission.
//! Metadata and saved totals have independent cadences and read watermarks.
use super::inspection_read::{Dependencies, PreparedInspection};
use crate::{
    diagnostics::Diagnostics,
    protocol::{ApiError, ErrorCode},
    ui::{CommandSubmission, InspectionPart, InspectionTarget, ManagementContext},
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const METADATA_PERIOD: Duration = Duration::from_millis(500);
const USAGE_PERIOD: Duration = Duration::from_secs(3);

fn has_usage(target: &InspectionTarget) -> bool {
    matches!(
        target,
        InspectionTarget::Usage(_)
            | InspectionTarget::Thread(_)
            | InspectionTarget::Run(_)
            | InspectionTarget::Turn(_)
    )
}
fn global_saved(target: &InspectionTarget) -> bool {
    matches!(
        target,
        InspectionTarget::Storage
            | InspectionTarget::Deletion(_)
            | InspectionTarget::Index(_)
            | InspectionTarget::HistorySources(_)
    )
}

#[derive(Clone)]
pub(super) struct Ticket {
    pub cursor: Option<String>,
    pub generation: u64,
    pub context: ManagementContext,
    pub target: InspectionTarget,
    pub part: InspectionPart,
    pub stop: CancellationToken,
    pub submission: Option<CommandSubmission>,
    serial: u64,
}
#[derive(Clone)]
pub(super) struct UsageTicket {
    pub identity: Ticket,
    pub diagnostics: Diagnostics,
    pub context: crate::usage_totals::ReadContext,
}
pub(super) struct RawUsage {
    pub value: Value,
    pub context_usage: Option<crate::compaction::context_usage::RawContextUsage>,
    pub observed_at: String,
}
pub(super) struct PreparedUsage {
    pub value: Value,
    pub context_usage: Option<Value>,
    pub context_scope: Option<(String, String, String)>,
    pub observed_at: String,
}
#[derive(Clone)]
pub(super) struct Facts {
    pub value: Arc<Value>,
    pub sequence: u64,
    pub observed_at: String,
    pub context_usage: Option<Arc<Value>>,
    pub context_scope: Option<(String, String, String)>,
}
pub(super) struct Failure {
    pub target: InspectionTarget,
    pub part: InspectionPart,
    pub read_sequence: u64,
    pub observed_at: String,
    pub error: String,
}
pub(super) enum Completion {
    Retired,
    Updated,
    Failed(Box<Failure>),
}
#[derive(Clone)]
pub(super) struct Presentation {
    pub last_coverage: Option<Facts>,
    pub cursor: Option<String>,
    pub physical: Option<Arc<Value>>,
    pub generation: u64,
    pub context: ManagementContext,
    pub target: InspectionTarget,
    pub metadata: Facts,
    pub usage: Option<Facts>,
    pub limited_fields: Vec<String>,
    pub stop: CancellationToken,
    version: u64,
}
struct Scope {
    output_marker: Option<std::time::Instant>,
    cursor: Option<String>,
    metadata_cursor: Option<Option<String>>,
    awaiting_window: bool,
    last_coverage: Option<Facts>,
    generation: u64,
    context: ManagementContext,
    target: InspectionTarget,
    visible: bool,
    stop: CancellationToken,
    dependencies: Option<Dependencies>,
    metadata_dirty: bool,
    usage_dirty: bool,
    metadata_inflight: Option<u64>,
    usage_inflight: Option<u64>,
    metadata_next: Option<Instant>,
    usage_next: Option<Instant>,
    admitted_at: Instant,
    metadata: Option<Facts>,
    usage: Option<Facts>,
    limited_fields: Vec<String>,
    version: u64,
    prepared_version: Option<u64>,
    presenting: Option<u64>,
    pending_submission: Option<CommandSubmission>,
}
struct VisibleScope {
    context: ManagementContext,
    target: InspectionTarget,
}
struct Visibility {
    revision: u64,
    desired: Option<VisibleScope>,
}
#[derive(Default)]
pub(super) struct Observation {
    visibility: Option<Visibility>,
    physical: super::inspection_physical::Physical,
    scope: Option<Scope>,
    serial: u64,
    retired_submission: Option<CommandSubmission>,
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.close();
    }
}
impl Observation {
    pub fn close(&mut self) {
        self.physical.hide();
        if let Some(mut scope) = self.scope.take() {
            scope.stop.cancel();
            if let Some(submission) = scope.pending_submission.take() {
                debug_assert!(
                    self.retired_submission.is_none(),
                    "frontend drains retired input before another action"
                );
                self.retired_submission = Some(submission);
            }
        }
    }
    pub fn take_retired_submission(&mut self) -> Option<CommandSubmission> {
        self.retired_submission.take()
    }
    pub fn install(
        &mut self,
        generation: u64,
        context: ManagementContext,
        target: InspectionTarget,
        submission: Option<CommandSubmission>,
    ) {
        self.close();
        let dependencies = match &target {
            InspectionTarget::Thread(id) | InspectionTarget::Attachments { thread_id: id, .. } => {
                Some(Dependencies {
                    thread: id.clone(),
                    parent_thread: None,
                })
            }
            _ => None,
        };
        let usage_dirty = has_usage(&target);
        let visible = self
            .visibility
            .as_ref()
            .and_then(|v| v.desired.as_ref())
            .is_some_and(|v| v.context == context && v.target == target);
        self.scope = Some(Scope {
            output_marker: None,
            cursor: None,
            metadata_cursor: None,
            awaiting_window: false,
            last_coverage: None,
            generation,
            context,
            target,
            visible,
            stop: CancellationToken::new(),
            dependencies,
            metadata_dirty: true,
            usage_dirty,
            metadata_inflight: None,
            usage_inflight: None,
            metadata_next: None,
            usage_next: None,
            admitted_at: Instant::now(),
            metadata: None,
            usage: None,
            limited_fields: Vec::new(),
            version: 0,
            prepared_version: None,
            presenting: None,
            pending_submission: submission,
        });
    }
    fn visibility_after(&self, revision: u64) -> bool {
        self.visibility.as_ref().is_none_or(|old| {
            let delta = revision.wrapping_sub(old.revision);
            delta != 0 && delta < (1_u64 << 63)
        })
    }
    pub fn hide_at(&mut self, revision: u64) {
        if !self.visibility_after(revision) {
            return;
        }
        self.visibility = Some(Visibility {
            revision,
            desired: None,
        });
        if let Some((context, target)) = self
            .scope
            .as_ref()
            .map(|s| (s.context.clone(), s.target.clone()))
        {
            self.apply_visibility(&context, &target, false);
        }
    }
    pub fn observe_at(
        &mut self,
        context: &ManagementContext,
        target: &InspectionTarget,
        visible: bool,
        revision: u64,
    ) {
        if !visible {
            self.hide_at(revision);
            return;
        }
        if !self.visibility_after(revision) {
            return;
        }
        self.visibility = Some(Visibility {
            revision,
            desired: Some(VisibleScope {
                context: context.clone(),
                target: target.clone(),
            }),
        });
        self.apply_visibility(context, target, true);
    }
    #[cfg(test)]
    pub fn observe(
        &mut self,
        context: &ManagementContext,
        target: &InspectionTarget,
        visible: bool,
    ) {
        let revision = self
            .visibility
            .as_ref()
            .map_or(1, |old| old.revision.wrapping_add(1).max(1));
        self.observe_at(context, target, visible, revision);
    }
    fn apply_visibility(
        &mut self,
        context: &ManagementContext,
        target: &InspectionTarget,
        visible: bool,
    ) {
        let Some(scope) = &mut self.scope else {
            return;
        };
        if &scope.context != context || &scope.target != target {
            return;
        }
        if !visible {
            scope.visible = false;
            scope.stop.cancel();
            self.physical.hide();
            scope.metadata_inflight = None;
            scope.usage_inflight = None;
            scope.presenting = None;
            if let Some(submission) = scope.pending_submission.take() {
                debug_assert!(self.retired_submission.is_none());
                self.retired_submission = Some(submission);
            }
            return;
        }
        let manual_refresh = scope.visible;
        if !scope.visible {
            scope.stop = CancellationToken::new();
            scope.admitted_at = Instant::now();
        }
        scope.visible = true;
        // Also serves explicit user refresh. UI emits no unchanged frame scope.
        scope.metadata_dirty = true;
        scope.usage_dirty = has_usage(&scope.target);
        if manual_refresh {
            if scope.cursor.is_some() {
                scope.version = scope.version.wrapping_add(1);
                scope.awaiting_window = true;
            }
            scope.cursor = None;
        }
        if matches!(scope.target, InspectionTarget::Storage) {
            self.physical.dirty();
        }
    }
    pub fn changed(&mut self, thread: &str) {
        let Some(scope) = &mut self.scope else {
            return;
        };
        if !scope.visible {
            return;
        }
        if let InspectionTarget::Lists(list) = &scope.target {
            let global = match &list.command {
                crate::protocol::Command::Status(args) => {
                    args.thread_id.is_none() && args.run_id.is_none()
                }
                crate::protocol::Command::ThreadList(_) => true,
                crate::protocol::Command::ShellList(args) => args.thread_id.is_none(),
                crate::protocol::Command::CompactList(args) => args.thread_id.is_none(),
                _ => false,
            };
            if global || list.threads.iter().any(|id| id == thread) {
                scope.metadata_dirty = true;
            }
            return;
        }
        if global_saved(&scope.target) {
            scope.metadata_dirty = true;
            if matches!(scope.target, InspectionTarget::Storage) {
                self.physical.dirty();
            }
            return;
        }
        if matches!(&scope.target, InspectionTarget::Usage(args) if args.include_children) {
            scope.usage_dirty = true;
            return;
        }
        if matches!(&scope.target, InspectionTarget::Usage(args) if args.group_by.is_some())
            && scope.dependencies.is_none()
        {
            // Grouped all-conversation usage has no fabricated viewer Thread.
            scope.usage_dirty = true;
            return;
        }
        if matches!(scope.target, InspectionTarget::Usage(_))
            && scope.dependencies.is_none()
            && scope.usage_inflight.is_some()
        {
            scope.usage_dirty = true;
        }
        if let Some(dependencies) = &scope.dependencies {
            if dependencies.thread == thread {
                scope.metadata_dirty = true;
                scope.usage_dirty = has_usage(&scope.target);
            } else if dependencies.parent_thread.as_deref() == Some(thread) {
                scope.metadata_dirty = true;
            }
        } else if scope.metadata_inflight.is_some() {
            // A single conservative invalidation closes initial dependency races.
            scope.metadata_dirty = true;
            scope.usage_dirty = has_usage(&scope.target);
            if matches!(scope.target, InspectionTarget::Storage) {
                self.physical.dirty();
            }
        }
    }
    pub fn resync(&mut self) {
        if let Some(scope) = &mut self.scope
            && scope.visible
        {
            scope.metadata_dirty = true;
            scope.usage_dirty = has_usage(&scope.target);
            if matches!(scope.target, InspectionTarget::Storage) {
                self.physical.dirty();
            }
        }
    }
    pub fn watches_index(&self) -> bool {
        self.scope.as_ref().is_some_and(|scope| {
            scope.visible
                && matches!(
                    scope.target,
                    InspectionTarget::Index(_) | InspectionTarget::Storage
                )
        })
    }
    pub fn watches_owner_state(&self) -> bool {
        self.scope.as_ref().is_some_and(|scope| {
            scope.visible
                && matches!(
                    scope.target,
                    InspectionTarget::Storage | InspectionTarget::Operation(_)
                )
        })
    }
    pub fn owner_state_hint(&mut self) {
        if let Some(scope) = self.scope.as_mut().filter(|scope| {
            scope.visible
                && matches!(
                    scope.target,
                    InspectionTarget::Storage | InspectionTarget::Operation(_)
                )
        }) {
            scope.metadata_dirty = true;
        }
    }
    pub fn output_notified(&mut self) {
        if let Some(scope) = self.scope.as_mut().filter(|scope| {
            scope.visible
                && matches!(
                    scope.target,
                    InspectionTarget::Storage | InspectionTarget::Index(_)
                )
        }) {
            scope.metadata_dirty = true;
            if matches!(scope.target, InspectionTarget::Storage) {
                self.physical.dirty();
            }
        }
    }
    pub fn output_execution(&self) -> Option<String> {
        let scope = self.scope.as_ref().filter(|scope| scope.visible)?;
        match &scope.target {
            InspectionTarget::Shell(id) => Some(id.clone()),
            _ => None,
        }
    }
    pub fn output_markers(&mut self, markers: &[(String, std::time::Instant)], prime: bool) {
        let Some(scope) = self.scope.as_mut().filter(|scope| scope.visible) else {
            return;
        };
        let InspectionTarget::Shell(id) = &scope.target else {
            return;
        };
        let next = markers
            .iter()
            .find(|(execution, _)| execution == id)
            .map(|(_, at)| *at);
        if let Some(next) = next {
            if !prime && Some(next) != scope.output_marker {
                scope.metadata_dirty = true;
            }
            scope.output_marker = Some(next);
        }
    }
    pub fn index_changed(&mut self) {
        if let Some(scope) = self.scope.as_mut().filter(|scope| {
            scope.visible
                && matches!(
                    scope.target,
                    InspectionTarget::Index(_) | InspectionTarget::Storage
                )
        }) {
            scope.metadata_dirty = true;
            if matches!(scope.target, InspectionTarget::Storage) {
                self.physical.dirty();
            }
        }
    }
    pub fn availability_changed(&mut self) {
        if let Some(scope) = self.scope.as_mut().filter(|scope| scope.visible) {
            scope.metadata_dirty = true;
            scope.usage_dirty = has_usage(&scope.target);
            if matches!(scope.target, InspectionTarget::Storage) {
                self.physical.dirty();
            }
        }
    }
    pub fn next_index_window(
        &mut self,
        context: &ManagementContext,
        target: &InspectionTarget,
    ) -> Result<(), ApiError> {
        let scope = self
            .scope
            .as_mut()
            .filter(|scope| {
                scope.visible
                    && &scope.context == context
                    && &scope.target == target
                    && matches!(scope.target, InspectionTarget::Index(_))
            })
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "The index inspection is no longer visible",
                )
            })?;
        if scope.metadata_inflight.is_some() {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "Index facts are refreshing; retry after the current observation",
            ));
        }
        let cursor = scope
            .metadata
            .as_ref()
            .and_then(|facts| {
                facts
                    .value
                    .pointer("/management/coverage_window/next_cursor")
            })
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::InvalidState,
                    "There is no next coverage window in this observation",
                )
            })?;
        scope.cursor = Some(cursor.into());
        scope.awaiting_window = true;
        scope.metadata_dirty = true;
        Ok(())
    }
    pub fn physical_deadline(&self) -> Option<Instant> {
        self.scope
            .as_ref()
            .filter(|scope| scope.visible && matches!(scope.target, InspectionTarget::Storage))
            .and_then(|_| self.physical.deadline())
    }
    pub fn take_physical(&mut self, now: Instant) -> Option<super::inspection_physical::Ticket> {
        let scope = self
            .scope
            .as_mut()
            .filter(|scope| scope.visible && matches!(scope.target, InspectionTarget::Storage))?;
        self.physical
            .take(now, scope.generation, scope.context.clone(), &scope.stop)
    }
    pub fn complete_physical(
        &mut self,
        ticket: &super::inspection_physical::Ticket,
        result: Result<Value, ApiError>,
    ) -> bool {
        let Some(scope) = self.scope.as_mut().filter(|scope| {
            scope.visible
                && scope.generation == ticket.generation
                && scope.context == ticket.context
                && matches!(scope.target, InspectionTarget::Storage)
        }) else {
            return false;
        };
        if !self.physical.complete(ticket, result) {
            return false;
        }
        scope.version = scope.version.wrapping_add(1);
        true
    }
    pub fn deadline(&self) -> Option<Instant> {
        let scope = self.scope.as_ref().filter(|scope| scope.visible)?;
        let metadata = (!matches!(scope.target, InspectionTarget::Usage(_))
            && scope.metadata_dirty
            && scope.metadata_inflight.is_none())
        .then_some(scope.metadata_next.unwrap_or(scope.admitted_at));
        let usage = (scope.usage_dirty && scope.usage_inflight.is_none())
            .then_some(scope.usage_next.unwrap_or(scope.admitted_at));
        metadata.into_iter().chain(usage).min()
    }
    pub fn take_metadata(&mut self, now: Instant) -> Option<Ticket> {
        self.take(now, InspectionPart::Metadata, false)
    }
    pub fn take_initial_metadata(&mut self, now: Instant) -> Option<Ticket> {
        if matches!(self.scope.as_ref()?.target, InspectionTarget::Usage(_)) {
            return None;
        }
        self.scope.as_ref()?.pending_submission.as_ref()?;
        self.take(now, InspectionPart::Metadata, true)
    }
    pub fn take_usage(&mut self, now: Instant, diagnostics: Diagnostics) -> Option<UsageTicket> {
        self.usage_ticket(now, diagnostics, false)
    }
    pub fn take_initial_usage(
        &mut self,
        now: Instant,
        diagnostics: Diagnostics,
    ) -> Option<UsageTicket> {
        let scope = self.scope.as_ref()?;
        if !has_usage(&scope.target)
            || !(scope.pending_submission.is_some()
                || (scope.metadata.is_none()
                    && scope.metadata_inflight.is_some()
                    && scope.usage.is_none()))
        {
            return None;
        }
        self.usage_ticket(now, diagnostics, true)
    }
    fn usage_ticket(
        &mut self,
        now: Instant,
        diagnostics: Diagnostics,
        initial: bool,
    ) -> Option<UsageTicket> {
        let identity = self.take(now, InspectionPart::Usage, initial)?;
        let mut context = crate::usage_totals::read_context();
        context.trigger = if self.scope.as_ref()?.usage.is_none() {
            crate::usage_totals::UsageTrigger::Open
        } else {
            crate::usage_totals::UsageTrigger::Changed
        };
        Some(UsageTicket {
            identity,
            diagnostics,
            context,
        })
    }
    fn take(&mut self, now: Instant, part: InspectionPart, initial: bool) -> Option<Ticket> {
        let scope = self.scope.as_mut().filter(|scope| {
            !(part == InspectionPart::Metadata
                && matches!(scope.target, InspectionTarget::Usage(_)))
                && (scope.visible
                    || (initial
                        && (scope.pending_submission.is_some()
                            || (part == InspectionPart::Usage
                                && scope.metadata.is_none()
                                && scope.metadata_inflight.is_some()))))
                && !scope.stop.is_cancelled()
        })?;
        let (dirty, inflight, next) = match part {
            InspectionPart::Metadata => (
                &mut scope.metadata_dirty,
                &mut scope.metadata_inflight,
                &mut scope.metadata_next,
            ),
            InspectionPart::Usage => (
                &mut scope.usage_dirty,
                &mut scope.usage_inflight,
                &mut scope.usage_next,
            ),
        };
        if !*dirty || inflight.is_some() || next.is_some_and(|due| due > now) {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        *inflight = Some(self.serial);
        *dirty = false;
        *next = Some(
            now + match part {
                InspectionPart::Metadata => METADATA_PERIOD,
                InspectionPart::Usage => USAGE_PERIOD,
            },
        );
        let submission = if part == InspectionPart::Metadata
            || matches!(scope.target, InspectionTarget::Usage(_))
        {
            scope.pending_submission.take()
        } else {
            None
        };
        let stop = if submission.is_some() {
            CancellationToken::new()
        } else {
            scope.stop.child_token()
        };
        Some(Ticket {
            cursor: scope.cursor.clone(),
            generation: scope.generation,
            context: scope.context.clone(),
            target: scope.target.clone(),
            part,
            stop,
            submission,
            serial: self.serial,
        })
    }
    fn accepts(&self, ticket: &Ticket) -> bool {
        !ticket.stop.is_cancelled()
            && self.scope.as_ref().is_some_and(|scope| {
                scope.visible
                    && scope.generation == ticket.generation
                    && scope.context == ticket.context
                    && scope.target == ticket.target
                    && match ticket.part {
                        InspectionPart::Metadata => scope.metadata_inflight == Some(ticket.serial),
                        InspectionPart::Usage => scope.usage_inflight == Some(ticket.serial),
                    }
            })
    }
    pub fn complete_metadata(
        &mut self,
        ticket: &Ticket,
        sequence: u64,
        result: Result<PreparedInspection, ApiError>,
    ) -> Completion {
        if ticket.part != InspectionPart::Metadata || !self.accepts(ticket) {
            return Completion::Retired;
        }
        let scope = self.scope.as_mut().expect("accepted scope");
        scope.metadata_inflight = None;
        if ticket.cursor != scope.cursor {
            scope.metadata_dirty = true;
            return Completion::Retired;
        }
        let result = result.and_then(|prepared| {
            if prepared.target == ticket.target {
                Ok(prepared)
            } else {
                Err(ApiError::new(
                    ErrorCode::InvalidState,
                    "Inspection target changed during preparation",
                ))
            }
        });
        match result {
            Ok(prepared) => {
                if scope
                    .metadata
                    .as_ref()
                    .is_some_and(|old| old.sequence > sequence)
                {
                    return Completion::Retired;
                }
                scope.dependencies = Some(prepared.dependencies);
                scope.metadata = Some(Facts {
                    value: Arc::new(prepared.value),
                    sequence,
                    observed_at: prepared.observed_at,
                    context_usage: None,
                    context_scope: None,
                });
                scope.metadata_cursor = Some(ticket.cursor.clone());
                scope.awaiting_window = false;
                if matches!(scope.target, InspectionTarget::Index(_))
                    && scope.metadata.as_ref().is_some_and(|facts| {
                        facts
                            .value
                            .pointer("/management/coverage_window/kind")
                            .and_then(Value::as_str)
                            == Some("index_coverage_window")
                    })
                {
                    scope.last_coverage = scope.metadata.clone();
                }
                scope.limited_fields = prepared.limited_fields;
                scope.version = scope.version.wrapping_add(1);
                Completion::Updated
            }
            Err(error) => Completion::Failed(Box::new(Failure {
                target: ticket.target.clone(),
                part: ticket.part,
                read_sequence: sequence,
                observed_at: chrono::Utc::now().to_rfc3339(),
                error: error.to_string(),
            })),
        }
    }
    pub fn complete_usage(
        &mut self,
        ticket: &UsageTicket,
        sequence: u64,
        result: Result<PreparedUsage, ApiError>,
    ) -> Completion {
        let identity = &ticket.identity;
        // An admitted input's read survives its display. Even if rendering was
        // retired, retain completion-based heavy-read cadence for this scope.
        if identity.submission.is_some()
            && let Some(scope) = self.scope.as_mut()
            && scope.generation == identity.generation
            && scope.context == identity.context
            && scope.target == identity.target
        {
            scope.usage_next = Some(Instant::now() + USAGE_PERIOD);
        }
        if identity.part != InspectionPart::Usage || !self.accepts(identity) {
            return Completion::Retired;
        }
        let scope = self.scope.as_mut().expect("accepted scope");
        scope.usage_inflight = None;
        scope.usage_next = Some(Instant::now() + USAGE_PERIOD);
        match result {
            Ok(raw) => {
                if scope
                    .usage
                    .as_ref()
                    .is_some_and(|old| old.sequence > sequence)
                {
                    return Completion::Retired;
                }
                scope.usage = Some(Facts {
                    value: Arc::new(raw.value),
                    sequence,
                    observed_at: raw.observed_at,
                    context_usage: raw.context_usage.map(Arc::new),
                    context_scope: raw.context_scope,
                });
                if let InspectionTarget::Usage(args) = &scope.target {
                    let facts = scope.usage.as_ref().expect("stored usage");
                    let thread = args
                        .thread_id
                        .as_deref()
                        .or_else(|| facts.value["thread_id"].as_str());
                    scope.dependencies = thread.map(|thread| Dependencies {
                        thread: thread.to_owned(),
                        parent_thread: None,
                    });
                    scope.metadata = scope.usage.clone();
                    scope.metadata_cursor = Some(None);
                }
                scope.version = scope.version.wrapping_add(1);
                Completion::Updated
            }
            Err(error) => Completion::Failed(Box::new(Failure {
                target: identity.target.clone(),
                part: identity.part,
                read_sequence: sequence,
                observed_at: chrono::Utc::now().to_rfc3339(),
                error: error.to_string(),
            })),
        }
    }
    pub fn take_presentation(&mut self) -> Option<Presentation> {
        let scope = self
            .scope
            .as_mut()
            .filter(|scope| scope.visible && !scope.stop.is_cancelled())?;
        let metadata = scope.metadata.clone()?;
        if scope.awaiting_window || scope.metadata_cursor.as_ref() != Some(&scope.cursor) {
            return None;
        }
        if scope.presenting.is_some() || scope.prepared_version == Some(scope.version) {
            return None;
        }
        scope.presenting = Some(scope.version);
        Some(Presentation {
            physical: if matches!(scope.target, InspectionTarget::Storage) {
                self.physical.facts()
            } else {
                None
            },
            last_coverage: if matches!(scope.target, InspectionTarget::Index(_)) {
                scope.last_coverage.clone()
            } else {
                None
            },
            cursor: scope.cursor.clone(),
            generation: scope.generation,
            context: scope.context.clone(),
            target: scope.target.clone(),
            metadata,
            usage: scope.usage.clone(),
            limited_fields: scope.limited_fields.clone(),
            stop: scope.stop.child_token(),
            version: scope.version,
        })
    }
    pub fn presentation_finished(&mut self, ticket: &Presentation) -> bool {
        let Some(scope) = &mut self.scope else {
            return false;
        };
        if ticket.stop.is_cancelled()
            || scope.generation != ticket.generation
            || scope.context != ticket.context
            || scope.target != ticket.target
            || scope.presenting != Some(ticket.version)
        {
            return false;
        }
        scope.presenting = None;
        if !scope.visible
            || scope.version != ticket.version
            || scope.cursor != ticket.cursor
            || scope.awaiting_window
            || scope.metadata_cursor.as_ref() != Some(&scope.cursor)
        {
            return false;
        }
        scope.prepared_version = Some(ticket.version);
        true
    }
    pub fn presentation_refused(&mut self, ticket: &Presentation) {
        if let Some(scope) = &mut self.scope
            && scope.generation == ticket.generation
            && scope.context == ticket.context
            && scope.target == ticket.target
            && scope.presenting == Some(ticket.version)
        {
            scope.presenting = None;
        }
    }
}

pub(super) fn read_usage_tx(
    db: &rusqlite::Connection,
    ticket: &UsageTicket,
    instance: Option<&str>,
) -> Result<RawUsage, crate::storage::StorageError> {
    use crate::usage_totals::Scope;
    if let InspectionTarget::Usage(args) = &ticket.identity.target
        && args.group_by.is_some()
    {
        crate::protocol::Command::UsageGet(args.clone()).validate()?;
        let instance = instance.ok_or_else(|| {
            ApiError::new(
                ErrorCode::InvalidState,
                "Grouped usage owner identity is unavailable",
            )
        })?;
        return Ok(RawUsage {
            value: crate::usage_totals::breakdown::read(db, args, instance)?,
            context_usage: None,
            observed_at: chrono::Utc::now().to_rfc3339(),
        });
    }
    let scope = match &ticket.identity.target {
        InspectionTarget::Usage(args) => {
            let count = usize::from(args.thread_id.is_some())
                + usize::from(args.run_id.is_some())
                + usize::from(args.turn_id.is_some());
            if count != 1 || (args.include_children && args.thread_id.is_none()) {
                return Err(ApiError::new(ErrorCode::InvalidArgument, "Usage requires exactly one explicit Thread, Run or Turn; children requires Thread").into());
            }
            if let Some(id) = &args.turn_id {
                Scope::Turn(id.clone())
            } else if let Some(id) = &args.run_id {
                Scope::Run(id.clone())
            } else {
                Scope::Thread {
                    id: args.thread_id.clone().expect("explicit scope"),
                    children: args.include_children,
                }
            }
        }
        InspectionTarget::Thread(id) => Scope::Thread {
            id: id.clone(),
            children: false,
        },
        InspectionTarget::Run(id) => Scope::Run(id.clone()),
        InspectionTarget::Turn(id) => Scope::Turn(id.clone()),
        _ => {
            return Err(ApiError::new(
                ErrorCode::InvalidState,
                "This inspection has no model usage scope",
            )
            .into());
        }
    };
    let value = match &ticket.identity.target {
        InspectionTarget::Usage(args) => {
            let window =
                crate::readback::Window::new(args.since.as_deref(), args.until.as_deref())?;
            crate::usage_totals::read_period_logged(
                db,
                &scope,
                ticket.context,
                Some(&ticket.diagnostics),
                &window,
            )?
        }
        _ => {
            crate::usage_totals::read_logged(db, &scope, ticket.context, Some(&ticket.diagnostics))?
        }
    };
    let context_usage = match &ticket.identity.target {
        InspectionTarget::Thread(id) => Some(
            crate::compaction::context_usage::read_context_usage_tx(db, id, Some(512))?,
        ),
        _ => None,
    };
    Ok(RawUsage {
        value,
        context_usage,
        observed_at: chrono::Utc::now().to_rfc3339(),
    })
}
pub(super) fn prepare_usage(raw: RawUsage) -> PreparedUsage {
    let (context_usage, context_scope) = raw
        .context_usage
        .map(crate::compaction::context_usage::prepare_context_usage)
        .map(|(value, scope)| (Some(value), Some(scope)))
        .unwrap_or((None, None));
    PreparedUsage {
        value: raw.value,
        context_usage,
        context_scope,
        observed_at: raw.observed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(epoch: u64) -> ManagementContext {
        ManagementContext {
            epoch,
            owner_draft: "draft".into(),
            owner_thread: Some("viewer".into()),
        }
    }
    fn begin() -> Observation {
        let mut value = Observation::default();
        value.install(
            1,
            context(1),
            InspectionTarget::Run("foreign-run".into()),
            None,
        );
        value.observe(
            &context(1),
            &InspectionTarget::Run("foreign-run".into()),
            true,
        );
        value
    }
    fn facts(state: &str) -> PreparedInspection {
        PreparedInspection {
            target: InspectionTarget::Run("foreign-run".into()),
            dependencies: Dependencies {
                thread: "foreign".into(),
                parent_thread: None,
            },
            value: serde_json::json!({"state":state,"run_id":"foreign-run"}),
            observed_at: "2026-10-31T00:00:00Z".into(),
            limited_fields: vec![],
        }
    }
    fn logger() -> (tempfile::TempDir, Diagnostics) {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::start(root.path().into(), crate::config::Logging::default());
        (root, diagnostics)
    }

    #[test]
    fn index_window_survives_invalidations_and_cover_until_explicit_refresh() {
        let mut value = Observation::default();
        let target = InspectionTarget::Index(crate::protocol::IndexArgs::default());
        value.install(1, context(1), target.clone(), None);
        value.observe(&context(1), &target, true);
        let at = Instant::now();
        let result = || PreparedInspection {
            target: target.clone(),
            dependencies: Dependencies {
                thread: String::new(),
                parent_thread: None,
            },
            value: serde_json::json!({"management":{"coverage_window":{"next_cursor":"next-position"}}}),
            observed_at: "read-time".into(),
            limited_fields: vec![],
        };
        let first = value.take_metadata(at).unwrap();
        value.complete_metadata(&first, 1, Ok(result()));
        value.next_index_window(&context(1), &target).unwrap();
        let next = value.take_metadata(at + METADATA_PERIOD).unwrap();
        assert_eq!(next.cursor.as_deref(), Some("next-position"));
        value.complete_metadata(&next, 2, Ok(result()));
        value.index_changed();
        value.changed("unrelated");
        value.availability_changed();
        value.resync();
        let current = value.take_metadata(at + METADATA_PERIOD * 2).unwrap();
        assert_eq!(
            current.cursor, next.cursor,
            "saved changes must preserve the selected membership window"
        );
        value.complete_metadata(&current, 3, Ok(result()));
        value.observe(&context(1), &target, false);
        value.observe(&context(1), &target, true);
        let resumed = value.take_metadata(at + METADATA_PERIOD * 3).unwrap();
        assert_eq!(resumed.cursor, next.cursor);
        value.complete_metadata(&resumed, 4, Ok(result()));
        value.observe(&context(1), &target, true);
        assert!(
            value
                .take_metadata(at + METADATA_PERIOD * 4)
                .unwrap()
                .cursor
                .is_none(),
            "only explicit refresh restarts the first window"
        );
        assert!(value.physical_deadline().is_none());
        let (_root, diagnostics) = logger();
        assert!(value.take_usage(at + USAGE_PERIOD, diagnostics).is_none());
    }

    #[test]
    fn unrelated_output_markers_do_not_admit_a_shell_read() {
        let mut value = Observation::default();
        let target = InspectionTarget::Shell("selected-execution".into());
        value.install(1, context(1), target.clone(), None);
        value.observe(&context(1), &target, true);
        value.output_markers(&[], true);
        let at = Instant::now();
        let first = value.take_metadata(at).unwrap();
        value.complete_metadata(
            &first,
            1,
            Ok(PreparedInspection {
                target: target.clone(),
                dependencies: Dependencies {
                    thread: "foreign".into(),
                    parent_thread: None,
                },
                value: serde_json::json!({"execution_id":"selected-execution","state":"running"}),
                observed_at: "read".into(),
                limited_fields: vec![],
            }),
        );
        for _ in 0..100 {
            value.output_markers(
                &[("hidden-execution".into(), std::time::Instant::now())],
                false,
            );
        }
        assert!(
            value.take_metadata(at + METADATA_PERIOD).is_none(),
            "unrelated output must not cause a DB read"
        );
        value.output_markers(
            &[("selected-execution".into(), std::time::Instant::now())],
            false,
        );
        assert!(value.take_metadata(at + METADATA_PERIOD).is_some());
        value.observe(&context(1), &target, false);
        assert!(value.output_execution().is_none());
        assert!(value.physical_deadline().is_none());
    }

    #[test]
    fn coverage_navigation_remains_available_after_the_general_field_limit() {
        let mut value = Observation::default();
        let target = InspectionTarget::Index(crate::protocol::IndexArgs::default());
        value.install(1, context(1), target.clone(), None);
        value.observe(&context(1), &target, true);
        let first = value.take_metadata(Instant::now()).unwrap();
        let source = serde_json::json!({"management":{"candidate":{"many":(0..160).collect::<Vec<_>>()},"coverage_window":{"next_cursor":"private-position"}}});
        value.complete_metadata(
            &first,
            1,
            Ok(PreparedInspection {
                target,
                dependencies: Dependencies {
                    thread: String::new(),
                    parent_thread: None,
                },
                value: source,
                observed_at: "read".into(),
                limited_fields: vec![],
            }),
        );
        let presentation = value.take_presentation().unwrap();
        let prepared =
            super::super::inspection_presentation::prepare(&presentation, 1024 * 1024).unwrap();
        assert!(
            prepared.snapshot.fields.len() <= crate::presentation::MAX_FIELDS + 1,
            "preserve the existing full-detail navigation row in addition to bounded metadata fields"
        );
        let field = prepared
            .snapshot
            .fields
            .iter()
            .find(|field| field.key == "/management/coverage_window/next_cursor")
            .unwrap();
        assert!(
            field.value_omitted,
            "the navigation hint must not masquerade as the full cursor value"
        );
        assert_eq!(
            prepared.report.detail.unwrap()["management"]["coverage_window"]["next_cursor"],
            "private-position"
        );
    }

    #[test]
    fn admitted_slash_read_keeps_input_outcome_when_display_is_covered_or_replaced() {
        let mut value = Observation::default();
        let target = InspectionTarget::Run("foreign-run".into());
        value.install(
            1,
            context(1),
            target.clone(),
            Some(CommandSubmission {
                draft_id: "input".into(),
                revision: 7,
            }),
        );
        let first = value.take_initial_metadata(Instant::now()).unwrap();
        assert!(first.submission.is_some());
        assert!(value.take_initial_metadata(Instant::now()).is_none());
        value.observe(&context(1), &target, false);
        assert!(
            !first.stop.is_cancelled(),
            "closing a display does not cancel the admitted input outcome"
        );
        assert!(value.deadline().is_none());
        value.install(2, context(2), InspectionTarget::Turn("other".into()), None);
        assert!(!first.stop.is_cancelled());
        assert!(
            matches!(
                value.complete_metadata(&first, 1, Ok(facts("live"))),
                Completion::Retired
            ),
            "the owned input result still cannot repaint the replaced display"
        );
        assert!(
            value.take_retired_submission().is_none(),
            "a submitted read must not restore its slash as a cancelled input"
        );
    }

    #[test]
    fn hidden_before_admission_retires_input_once_without_cancelling_a_later_read() {
        let mut value = Observation::default();
        let target = InspectionTarget::Run("foreign-run".into());
        value.install(
            1,
            context(1),
            target.clone(),
            Some(CommandSubmission {
                draft_id: "input-owner".into(),
                revision: 9,
            }),
        );
        value.observe(&context(1), &target, true);
        value.observe(&context(1), &target, false);
        assert!(value.deadline().is_none());
        let retired = value.take_retired_submission().unwrap();
        assert_eq!(retired.draft_id, "input-owner");
        assert_eq!(retired.revision, 9);
        assert!(value.take_retired_submission().is_none());
        value.observe(&context(1), &target, true);
        assert!(
            value
                .take_metadata(Instant::now())
                .unwrap()
                .submission
                .is_none()
        );
        value.close();
        assert!(value.take_retired_submission().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn fast_facts_advance_while_old_usage_is_inflight_without_rolling_metadata_back() {
        let at = Instant::now();
        let (_root, diagnostics) = logger();
        let mut value = begin();
        let first = value.take_metadata(at).unwrap();
        let usage = value.take_usage(at, diagnostics.clone()).unwrap();
        assert!(matches!(
            value.complete_metadata(&first, 10, Ok(facts("live"))),
            Completion::Updated
        ));
        value.changed("foreign");
        assert!(
            value
                .take_metadata(at + Duration::from_millis(499))
                .is_none()
        );
        tokio::time::advance(METADATA_PERIOD).await;
        let next = value.take_metadata(Instant::now()).unwrap();
        assert!(
            value
                .take_usage(at + METADATA_PERIOD, diagnostics.clone())
                .is_none()
        );
        value.complete_metadata(&next, 20, Ok(facts("stopped")));
        value.complete_usage(
            &usage,
            11,
            Ok(PreparedUsage {
                value: serde_json::json!({"known_sum":10}),
                context_usage: None,
                context_scope: None,
                observed_at: "older usage".into(),
            }),
        );
        let prepared = value.take_presentation().unwrap();
        assert_eq!(prepared.metadata.sequence, 20);
        assert_eq!(prepared.metadata.value["state"], "stopped");
        assert_eq!(prepared.usage.unwrap().sequence, 11);
        // Heavy cadence starts at completion; simulate that clock rather than
        // passing future take() timestamps while completion still sees the past.
        tokio::time::advance(USAGE_PERIOD - Duration::from_millis(100)).await;
        assert!(
            value
                .take_usage(Instant::now(), diagnostics.clone())
                .is_none()
        );
        tokio::time::advance(Duration::from_millis(100)).await;
        assert!(value.take_usage(Instant::now(), diagnostics).is_some());
    }
    #[test]
    fn first_read_race_verifies_once_then_unrelated_threads_are_dormant() {
        let mut value = begin();
        let at = Instant::now();
        let first = value.take_metadata(at).unwrap();
        value.changed("viewer");
        value.complete_metadata(&first, 1, Ok(facts("live")));
        let verification = value.take_metadata(at + METADATA_PERIOD).unwrap();
        value.complete_metadata(&verification, 2, Ok(facts("live")));
        for _ in 0..100 {
            value.changed("viewer");
        }
        assert!(value.take_metadata(at + METADATA_PERIOD * 2).is_none());
        value.changed("foreign");
        assert!(value.take_metadata(at + METADATA_PERIOD * 2).is_some());
    }
    #[test]
    fn covered_or_replaced_scope_cannot_accept_or_rearm_old_replies_and_cadence_survives_resume() {
        let mut value = begin();
        let at = Instant::now();
        let ticket = value.take_metadata(at).unwrap();
        value.observe(&context(1), &ticket.target, false);
        assert!(ticket.stop.is_cancelled());
        assert!(value.deadline().is_none());
        assert!(matches!(
            value.complete_metadata(&ticket, 9, Ok(facts("stopped"))),
            Completion::Retired
        ));
        value.observe(&context(1), &ticket.target, true);
        assert!(
            value
                .take_metadata(at + Duration::from_millis(499))
                .is_none()
        );
        assert!(value.take_metadata(at + METADATA_PERIOD).is_some());
        value.install(
            2,
            context(2),
            InspectionTarget::Turn("new-turn".into()),
            None,
        );
        value.observe(
            &context(2),
            &InspectionTarget::Turn("new-turn".into()),
            true,
        );
        // This is the delayed original hide (revision2), not a fresh user hide.
        value.observe_at(&context(1), &ticket.target, false, 2);
        assert!(
            value.take_metadata(Instant::now()).is_some(),
            "old close cannot retire a new target"
        );
    }
    #[test]
    fn delayed_presentation_is_discarded_and_a_new_pair_is_prepared_without_another_database_read()
    {
        let mut value = begin();
        let at = Instant::now();
        let first = value.take_metadata(at).unwrap();
        value.complete_metadata(&first, 1, Ok(facts("live")));
        let old = value.take_presentation().unwrap();
        value.changed("foreign");
        let next = value.take_metadata(at + METADATA_PERIOD).unwrap();
        value.complete_metadata(&next, 2, Ok(facts("stopped")));
        assert!(!value.presentation_finished(&old));
        let current = value.take_presentation().unwrap();
        assert_eq!(current.metadata.sequence, 2);
        assert!(value.presentation_finished(&current));
        assert!(value.take_presentation().is_none());
        value.observe(&context(1), &first.target, true);
        assert!(
            value.take_presentation().is_none(),
            "manual refresh does not republish an unchanged old pair"
        );
    }
    #[test]
    fn read_failure_keeps_last_facts_and_has_no_timer_polling_retry() {
        let mut value = begin();
        let at = Instant::now();
        let first = value.take_metadata(at).unwrap();
        value.complete_metadata(&first, 1, Ok(facts("live")));
        value.changed("foreign");
        let next = value.take_metadata(at + METADATA_PERIOD).unwrap();
        assert!(matches!(
            value.complete_metadata(
                &next,
                2,
                Err(ApiError::new(
                    ErrorCode::TargetUnavailable,
                    "source unavailable"
                ))
            ),
            Completion::Failed(_)
        ));
        assert!(value.take_metadata(at + Duration::from_secs(30)).is_none());
        let saved = value.take_presentation().unwrap();
        assert_eq!(saved.metadata.sequence, 1);
        assert_eq!(saved.metadata.value["state"], "live");
    }
    fn index_facts(target: &InspectionTarget, state: &str, coverage: Value) -> PreparedInspection {
        PreparedInspection {
            target: target.clone(),
            dependencies: Dependencies {
                thread: String::new(),
                parent_thread: None,
            },
            value: serde_json::json!({"management":{"status":{"state":state},"coverage_window":coverage}}),
            observed_at: format!("read-{state}"),
            limited_fields: vec![],
        }
    }
    #[test]
    fn manual_first_window_retires_held_next_read_and_queued_next_presentation() {
        let mut value = Observation::default();
        let target = InspectionTarget::Index(crate::protocol::IndexArgs::default());
        value.install(1, context(1), target.clone(), None);
        value.observe(&context(1), &target, true);
        let at = Instant::now();
        let first = value.take_metadata(at).unwrap();
        value.complete_metadata(
            &first,
            1,
            Ok(index_facts(
                &target,
                "first",
                serde_json::json!({"kind":"index_coverage_window","next_cursor":"next"}),
            )),
        );
        let shown = value.take_presentation().unwrap();
        assert!(value.presentation_finished(&shown));
        value.next_index_window(&context(1), &target).unwrap();
        let held = value.take_metadata(at + METADATA_PERIOD).unwrap();
        assert_eq!(held.cursor.as_deref(), Some("next"));
        value.observe(&context(1), &target, true);
        assert!(matches!(
            value.complete_metadata(
                &held,
                2,
                Ok(index_facts(
                    &target,
                    "obsolete-next",
                    serde_json::json!({"kind":"index_coverage_window"})
                ))
            ),
            Completion::Retired
        ));
        assert!(
            value.take_presentation().is_none(),
            "do not republish old facts while a fresh requested window is pending"
        );
        let reset = value
            .take_metadata(at + 2 * METADATA_PERIOD)
            .expect("obsolete matching inflight slot must be retired");
        assert!(reset.cursor.is_none());
        value.complete_metadata(
            &reset,
            3,
            Ok(index_facts(
                &target,
                "new-first",
                serde_json::json!({"kind":"index_coverage_window","next_cursor":"next"}),
            )),
        );
        let shown = value.take_presentation().unwrap();
        assert_eq!(
            shown.metadata.value["management"]["status"]["state"],
            "new-first"
        );
        assert!(value.presentation_finished(&shown));
        value.next_index_window(&context(1), &target).unwrap();
        let next = value.take_metadata(at + 3 * METADATA_PERIOD).unwrap();
        value.complete_metadata(
            &next,
            4,
            Ok(index_facts(
                &target,
                "next",
                serde_json::json!({"kind":"index_coverage_window"}),
            )),
        );
        let queued = value.take_presentation().unwrap();
        value.observe(&context(1), &target, true);
        assert!(!value.presentation_finished(&queued));
        assert!(value.take_presentation().is_none());
        assert!(
            value
                .take_metadata(at + 4 * METADATA_PERIOD)
                .unwrap()
                .cursor
                .is_none()
        );
    }
    #[test]
    fn physical_cache_sequence_and_cooldown_survive_storage_other_storage() {
        let mut value = Observation::default();
        let at = Instant::now();
        value.install(1, context(1), InspectionTarget::Storage, None);
        value.observe(&context(1), &InspectionTarget::Storage, true);
        let first = value.take_physical(at).unwrap();
        assert!(value.complete_physical(
            &first,
            Ok(serde_json::json!({"observed_at":"physical-original","database":{"bytes":13}}))
        ));
        let old = value.physical.facts().unwrap();
        let due = value.physical_deadline().unwrap_or_else(|| {
            value.physical.dirty();
            value.physical_deadline().unwrap()
        });
        value.install(2, context(2), InspectionTarget::Run("r".into()), None);
        value.observe(&context(2), &InspectionTarget::Run("r".into()), true);
        assert!(value.physical_deadline().is_none());
        assert!(value.take_physical(due).is_none());
        value.install(3, context(3), InspectionTarget::Storage, None);
        value.observe(&context(3), &InspectionTarget::Storage, true);
        assert_eq!(value.physical.sequence(), 1);
        assert!(Arc::ptr_eq(&old, &value.physical.facts().unwrap()));
        assert!(
            value
                .take_physical(due - Duration::from_millis(1))
                .is_none()
        );
        assert!(!value.complete_physical(&first, Ok(serde_json::json!({"observed_at":"late"}))));
        let next = value.take_physical(due).unwrap();
        assert_eq!(next.generation, 3);
        assert!(value.complete_physical(&next, Ok(serde_json::json!({"observed_at":"new"}))));
        assert_eq!(value.physical.sequence(), 2);
    }
    #[test]
    fn absent_shell_marker_after_known_output_never_becomes_unrelated_read() {
        let mut value = Observation::default();
        let target = InspectionTarget::Shell("selected".into());
        value.install(1, context(1), target.clone(), None);
        value.observe(&context(1), &target, true);
        let mark = std::time::Instant::now();
        value.output_markers(&[("selected".into(), mark)], true);
        let at = Instant::now();
        let first = value.take_metadata(at).unwrap();
        value.complete_metadata(
            &first,
            1,
            Ok(PreparedInspection {
                target: target.clone(),
                dependencies: Dependencies {
                    thread: "owner".into(),
                    parent_thread: None,
                },
                value: serde_json::json!({"state":"exited"}),
                observed_at: "read".into(),
                limited_fields: vec![],
            }),
        );
        for _ in 0..100 {
            value.output_markers(&[("other".into(), mark)], false);
            value.output_markers(&[], false);
        }
        assert!(value.take_metadata(at + METADATA_PERIOD).is_none());
        value.output_markers(&[("selected".into(), mark)], false);
        assert!(value.take_metadata(at + METADATA_PERIOD).is_none());
        value.output_markers(
            &[("selected".into(), mark + Duration::from_millis(1))],
            false,
        );
        assert!(value.take_metadata(at + METADATA_PERIOD).is_some());
    }
    #[test]
    fn failed_coverage_keeps_one_shared_success_while_fresh_saved_status_advances() {
        let mut value = Observation::default();
        let target = InspectionTarget::Index(crate::protocol::IndexArgs::default());
        value.install(1, context(1), target.clone(), None);
        value.observe(&context(1), &target, true);
        let at = Instant::now();
        let first = value.take_metadata(at).unwrap();
        value.complete_metadata(
            &first,
            10,
            Ok(index_facts(
                &target,
                "initial",
                serde_json::json!({"kind":"index_coverage_window","next_cursor":"n"}),
            )),
        );
        let first = value.take_presentation().unwrap();
        assert!(Arc::ptr_eq(
            &first.metadata.value,
            &first.last_coverage.as_ref().unwrap().value
        ));
        let cached = first.last_coverage.as_ref().unwrap().value.clone();
        assert!(value.presentation_finished(&first));
        value.index_changed();
        let next = value.take_metadata(at + METADATA_PERIOD).unwrap();
        assert!(matches!(
            value.complete_metadata(
                &next,
                11,
                Ok(index_facts(
                    &target,
                    "fresh-lifecycle",
                    serde_json::json!({"available":false,"current":false,"error":"cursor invalid"})
                ))
            ),
            Completion::Updated
        ));
        let refreshed = value.take_presentation().unwrap();
        assert_eq!(
            refreshed.metadata.value["management"]["status"]["state"],
            "fresh-lifecycle"
        );
        assert_eq!(
            refreshed.metadata.value["management"]["coverage_window"]["available"],
            false
        );
        assert_eq!(refreshed.last_coverage.as_ref().unwrap().sequence, 10);
        assert!(Arc::ptr_eq(
            &cached,
            &refreshed.last_coverage.as_ref().unwrap().value
        ));
        assert!(value.presentation_finished(&refreshed));
    }
}

#[cfg(test)]
mod standalone_usage_tests {
    use super::*;
    use crate::protocol::UsageGetArgs;
    fn context() -> ManagementContext {
        ManagementContext {
            epoch: 7,
            owner_draft: "input".into(),
            owner_thread: Some("viewer".into()),
        }
    }
    fn target(children: bool) -> InspectionTarget {
        InspectionTarget::Usage(UsageGetArgs {
            thread_id: Some("foreign".into()),
            include_children: children,
            ..Default::default()
        })
    }
    fn start(target: InspectionTarget, submission: bool) -> Observation {
        let mut value = Observation::default();
        value.install(
            1,
            context(),
            target.clone(),
            submission.then(|| CommandSubmission {
                draft_id: "input".into(),
                revision: 4,
            }),
        );
        value.observe(&context(), &target, true);
        value
    }
    fn diagnostics() -> (tempfile::TempDir, Diagnostics) {
        let root = tempfile::tempdir().unwrap();
        let value = Diagnostics::start(root.path().into(), crate::config::Logging::default());
        (root, value)
    }
    fn prepared() -> PreparedUsage {
        PreparedUsage {
            value: serde_json::json!({"thread_id":"foreign","scope":"thread","scope_id":"foreign","model_attempts":{}}),
            context_usage: None,
            context_scope: None,
            observed_at: "2026-10-31T00:00:00Z".into(),
        }
    }
    #[test]
    fn usage_only_shared_facts_root_projection_and_no_metadata_or_physical_work() {
        let (_root, diagnostics) = diagnostics();
        let mut value = start(target(false), false);
        let at = Instant::now();
        assert!(value.take_metadata(at).is_none());
        assert!(value.take_initial_metadata(at).is_none());
        assert!(value.take_physical(at).is_none());
        let ticket = value.take_usage(at, diagnostics.clone()).unwrap();
        assert!(matches!(
            value.complete_usage(&ticket, 10, Ok(prepared())),
            Completion::Updated
        ));
        let presentation = value.take_presentation().unwrap();
        assert!(Arc::ptr_eq(
            &presentation.metadata.value,
            &presentation.usage.as_ref().unwrap().value
        ));
        let report =
            super::super::inspection_presentation::prepare(&presentation, 1024 * 1024).unwrap();
        let body = report.report.detail.unwrap();
        assert_eq!(body["scope_id"], "foreign");
        assert!(body.get("usage_totals").is_none());
        assert!(body.get("context_usage").is_none());
        assert!(body.get("physical_observation").is_none());
        assert_eq!(report.snapshot.metadata_sequence, 10);
        assert_eq!(report.snapshot.usage_sequence, Some(10));
        assert!(
            value.deadline().is_none(),
            "metadata dirty cannot keep the event loop awake"
        );
        value.changed("viewer");
        assert!(
            value.deadline().is_none(),
            "viewer is not the observed scope"
        );
        value.changed("foreign");
        let due = value.scope.as_ref().unwrap().usage_next.unwrap();
        assert!(
            value
                .take_usage(due - Duration::from_nanos(1), diagnostics.clone())
                .is_none()
        );
        assert!(value.take_usage(due, diagnostics).is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn children_discovery_marks_one_usage_bit_and_cover_late_completion_cannot_reopen() {
        let at = Instant::now();
        let (_root, diagnostics) = diagnostics();
        let target = target(true);
        let mut value = start(target.clone(), false);
        let ticket = value.take_usage(at, diagnostics.clone()).unwrap();
        value.complete_usage(&ticket, 10, Ok(prepared()));
        value.changed("new-descendant-never-seen-before");
        tokio::time::advance(USAGE_PERIOD).await;
        let ticket = value
            .take_usage(Instant::now(), diagnostics.clone())
            .unwrap();
        value.observe(&context(), &target, false);
        assert!(matches!(
            value.complete_usage(&ticket, 11, Ok(prepared())),
            Completion::Retired
        ));
        assert!(value.take_presentation().is_none());
        assert!(value.deadline().is_none());
        value.observe(&context(), &target, true);
        assert!(
            value.take_usage(Instant::now(), diagnostics).is_none(),
            "cover/resume retains the heavy-read cooldown"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn initially_admitted_usage_owns_ack_and_errors_keep_last_facts() {
        let at = Instant::now();
        let (_root, diagnostics) = diagnostics();
        let mut value = start(target(false), true);
        let first = value.take_initial_usage(at, diagnostics.clone()).unwrap();
        assert_eq!(first.identity.submission.as_ref().unwrap().revision, 4);
        assert!(!first.identity.stop.is_cancelled());
        value.observe(&context(), &target(false), false);
        assert!(
            !first.identity.stop.is_cancelled(),
            "admitted input result survives display closure"
        );
        assert!(
            value.take_retired_submission().is_none(),
            "ticket owns the sole input outcome"
        );
        assert!(matches!(
            value.complete_usage(&first, 1, Ok(prepared())),
            Completion::Retired
        ));
        value.observe(&context(), &target(false), true);
        tokio::time::advance(USAGE_PERIOD).await;
        let next = value
            .take_usage(Instant::now(), diagnostics.clone())
            .unwrap();
        value.complete_usage(&next, 9, Ok(prepared()));
        let old = value
            .scope
            .as_ref()
            .unwrap()
            .metadata
            .as_ref()
            .unwrap()
            .value
            .clone();
        value.changed("foreign");
        tokio::time::advance(USAGE_PERIOD).await;
        let next = value.take_usage(Instant::now(), diagnostics).unwrap();
        assert!(matches!(
            value.complete_usage(
                &next,
                10,
                Err(ApiError::new(ErrorCode::InvalidState, "temporary"))
            ),
            Completion::Failed(_)
        ));
        assert!(Arc::ptr_eq(
            &old,
            &value
                .scope
                .as_ref()
                .unwrap()
                .metadata
                .as_ref()
                .unwrap()
                .value
        ));
    }
    #[test]
    fn explicit_scope_is_required_without_viewer_fallback() {
        let (_root, diagnostics) = diagnostics();
        let mut value = start(InspectionTarget::Usage(UsageGetArgs::default()), false);
        let ticket = value.take_usage(Instant::now(), diagnostics).unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        assert!(
            read_usage_tx(&db, &ticket, None).is_err(),
            "invalid scope rejected before any SQLite query"
        );
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;
    #[test]
    fn exact_attachment_thread_gc_coalescing_hidden_close_and_late_results() {
        let mut observer = Observation::default();
        let context = ManagementContext {
            epoch: 1,
            owner_draft: "draft".into(),
            owner_thread: Some("viewer".into()),
        };
        let target = InspectionTarget::Attachments {
            thread_id: "foreign".into(),
            attachment_id: None,
        };
        observer.install(1, context.clone(), target.clone(), None);
        let at = Instant::now();
        assert!(observer.take_metadata(at).is_none(), "not visible");
        observer.observe(&context, &target, true);
        let first = observer.take_metadata(at).unwrap();
        let prepared = || PreparedInspection {
            target: target.clone(),
            dependencies: Dependencies {
                thread: "foreign".into(),
                parent_thread: None,
            },
            value: serde_json::json!({"attachments":[]}),
            observed_at: "now".into(),
            limited_fields: vec![],
        };
        observer.complete_metadata(&first, 1, Ok(prepared()));
        observer.changed("viewer");
        assert!(
            observer
                .take_metadata(at + Duration::from_millis(500))
                .is_none(),
            "viewer does not own selection"
        );
        observer.changed("foreign");
        observer.availability_changed();
        observer.changed("foreign");
        assert!(
            observer
                .take_metadata(at + Duration::from_millis(499))
                .is_none()
        );
        let second = observer
            .take_metadata(at + Duration::from_millis(500))
            .unwrap();
        assert!(
            observer
                .take_metadata(at + Duration::from_secs(2))
                .is_none(),
            "one inflight metadata read"
        );
        observer.observe(&context, &target, false);
        observer.changed("foreign");
        observer.availability_changed();
        assert!(
            observer
                .take_metadata(at + Duration::from_secs(3))
                .is_none()
        );
        observer.close();
        assert!(matches!(
            observer.complete_metadata(&second, 2, Ok(prepared())),
            Completion::Retired
        ));
        assert!(
            observer
                .take_metadata(at + Duration::from_secs(4))
                .is_none()
        );
    }
}

#[cfg(test)]
mod operation_observation_tests {
    use super::*;
    #[test]
    fn receipt_hints_coalesce_at_metadata_cadence_and_hidden_scope_admits_nothing() {
        let context = ManagementContext {
            epoch: 1,
            owner_draft: "viewer-draft".into(),
            owner_thread: Some("viewer".into()),
        };
        let target = InspectionTarget::Operation("prior-instance:exact-id".into());
        let mut value = Observation::default();
        value.install(1, context.clone(), target.clone(), None);
        assert!(!value.watches_owner_state());
        assert!(value.take_metadata(Instant::now()).is_none());
        value.observe(&context, &target, true);
        assert!(value.watches_owner_state());
        let ticket = value.take_metadata(Instant::now()).unwrap();
        let facts = PreparedInspection {
            target: target.clone(),
            dependencies: Dependencies {
                thread: "actual".into(),
                parent_thread: None,
            },
            value: serde_json::json!({"operation_id":"prior-instance:exact-id","state":"pending"}),
            observed_at: "saved-read".into(),
            limited_fields: vec![],
        };
        assert!(matches!(
            value.complete_metadata(&ticket, 10, Ok(facts)),
            Completion::Updated
        ));
        let due = value.scope.as_ref().unwrap().metadata_next.unwrap();
        value.changed("viewer");
        assert!(value.deadline().is_none());
        for _ in 0..40 {
            value.owner_state_hint();
        }
        assert!(value.take_metadata(due - Duration::from_nanos(1)).is_none());
        let ticket = value.take_metadata(due).unwrap();
        assert!(value.take_metadata(due).is_none());
        let root = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::start(root.path().into(), crate::config::Logging::default());
        assert!(value.take_usage(due, diagnostics).is_none());
        assert!(value.take_physical(due).is_none());
        value.observe(&context, &target, false);
        value.owner_state_hint();
        value.changed("actual");
        assert!(!value.watches_owner_state());
        assert!(value.deadline().is_none());
        assert!(matches!(
            value.complete_metadata(
                &ticket,
                11,
                Err(ApiError::new(ErrorCode::OperationNotFound, "collected"))
            ),
            Completion::Retired
        ));
        assert!(value.take_presentation().is_none());
    }
}

#[cfg(test)]
mod visibility_order_tests {
    use super::*;
    fn context(epoch: u64) -> ManagementContext {
        ManagementContext {
            epoch,
            owner_thread: Some("viewer".into()),
            owner_draft: "d".into(),
        }
    }
    fn begin() -> Observation {
        let mut value = Observation::default();
        let target = InspectionTarget::Run("foreign-run".into());
        value.install(1, context(1), target.clone(), None);
        value.observe(&context(1), &target, true);
        value
    }
    #[test]
    fn priority_hide_rejects_older_show_even_when_it_overtakes_target_installation() {
        let mut value = begin();
        let target = InspectionTarget::Run("foreign-run".into());
        value.hide_at(3);
        value.observe_at(&context(1), &target, true, 2);
        value.changed("foreign");
        assert!(value.deadline().is_none());
        value.install(2, context(1), target.clone(), None);
        assert!(value.take_metadata(Instant::now()).is_none());
        value.observe_at(&context(1), &target, true, 4);
        assert!(value.take_metadata(Instant::now()).is_some());
        value.hide_at(5);
        value.observe_at(&context(1), &target, true, 4);
        assert!(value.deadline().is_none());
    }
    #[test]
    fn initial_lifecycle_metadata_and_usage_can_join_one_snapshot_admission() {
        let mut value = Observation::default();
        let target = InspectionTarget::Run("foreign-run".into());
        value.install(
            1,
            context(1),
            target,
            Some(CommandSubmission {
                draft_id: "d".into(),
                revision: 1,
            }),
        );
        let at = Instant::now();
        let metadata = value.take_initial_metadata(at).unwrap();
        let root = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::start(root.path().into(), crate::config::Logging::default());
        let usage = value.take_initial_usage(at, diagnostics).unwrap();
        assert_eq!(metadata.context, usage.identity.context);
        assert_eq!(metadata.generation, usage.identity.generation);
        assert_eq!(metadata.target, usage.identity.target);
        assert!(metadata.submission.is_some());
        assert!(usage.identity.submission.is_none());
    }
}

#[cfg(test)]
mod completion_size_tests {
    use super::*;
    #[test]
    fn boxed_failure_keeps_the_completion_enum_small_and_preserves_owned_facts() {
        assert!(std::mem::size_of::<Completion>() <= 2 * std::mem::size_of::<usize>());
        for part in [InspectionPart::Metadata, InspectionPart::Usage] {
            let target = InspectionTarget::Usage(crate::protocol::UsageGetArgs {
                group_by: Some("thread".into()),
                timezone: Some("UTC".into()),
                cursor: Some("original cursor".into()),
                ..Default::default()
            });
            let completion = Completion::Failed(Box::new(Failure {
                target: target.clone(),
                part,
                read_sequence: 42,
                observed_at: "captured failure time".into(),
                error: "original failure".into(),
            }));
            let Completion::Failed(failure) = completion else {
                panic!("failure variant");
            };
            let failure = *failure;
            assert_eq!(failure.target, target);
            assert_eq!(failure.part, part);
            assert_eq!(failure.read_sequence, 42);
            assert_eq!(failure.observed_at, "captured failure time");
            assert_eq!(failure.error, "original failure");
        }
    }
}

#[cfg(test)]
mod grouped_usage_reader_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn grouped_usage_uses_owner_bound_bounded_reader_and_stays_notification_driven() {
        let root = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::start(root.path().into(), crate::config::Logging::default());
        let args = crate::protocol::UsageGetArgs {
            group_by: Some("thread".into()),
            timezone: Some("UTC".into()),
            since: Some("2026-10-31T00:00:00Z".into()),
            until: Some("2026-11-01T00:00:00Z".into()),
            ..Default::default()
        };
        let target = InspectionTarget::Usage(args);
        let context = ManagementContext {
            epoch: 1,
            owner_draft: "draft".into(),
            owner_thread: Some("viewer-is-not-scope".into()),
        };
        let mut observation = Observation::default();
        observation.install(1, context.clone(), target.clone(), None);
        observation.observe(&context, &target, true);
        let ticket = observation
            .take_usage(Instant::now(), diagnostics.clone())
            .unwrap();
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
        db.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','now');
          INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','now');
          INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','now');
          INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,usage_json,started_at)VALUES('a','u',0,0,'p','m',0,'completed','{}','{\"input_tokens\":100}','2026-10-31T00:00:00+00:00');").unwrap();
        db.pragma_update(None, "query_only", true).unwrap();
        assert!(read_usage_tx(&db, &ticket, None).is_err());
        let raw = read_usage_tx(&db, &ticket, Some("owner-instance")).unwrap();
        assert_eq!(raw.value["items"][0]["key"], "t");
        assert_eq!(raw.value["items"][0]["input_per_request"]["mean"], 100.0);
        assert!(matches!(
            observation.complete_usage(&ticket, 1, Ok(prepare_usage(raw))),
            Completion::Updated
        ));
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        assert!(
            observation
                .take_usage(Instant::now(), diagnostics.clone())
                .is_none()
        );
        observation.changed("t");
        assert!(
            observation
                .take_usage(Instant::now(), diagnostics)
                .is_some()
        );
    }
}

#[cfg(test)]
mod retained_list_fence_tests {
    use super::*;
    use crate::protocol::*;
    use serde_json::json;

    #[tokio::test(start_paused = true)]
    async fn all_list_routes_keep_hidden_quiet_one_flight_and_retired_completion_fences() {
        let context = ManagementContext {
            epoch: 1,
            owner_draft: "fixture".into(),
            owner_thread: None,
        };
        for (command, row) in [
            (
                Command::Status(serde_json::from_value(json!({"thread_id":"scope"})).unwrap()),
                json!({"kind":"run","id":"r","thread_id":"scope"}),
            ),
            (
                Command::ThreadList(Default::default()),
                json!({"thread_id":"scope","title":"stored"}),
            ),
            (
                Command::ShellList(serde_json::from_value(json!({"thread_id":"scope"})).unwrap()),
                json!({"execution_id":"s","thread_id":"scope"}),
            ),
            (
                Command::CompactList(serde_json::from_value(json!({"thread_id":"scope"})).unwrap()),
                json!({"event_id":"operation:op","operation_ids":["op"],"thread_id":"scope"}),
            ),
        ] {
            let target = super::super::inspection_lists::capture_target(
                &command,
                Arc::new(json!({"items":[row]})),
            )
            .unwrap();
            let mut observation = Observation::default();
            observation.install(1, context.clone(), target.clone(), None);
            let now = Instant::now();
            assert!(
                observation.take_metadata(now).is_none(),
                "hidden scope cannot admit"
            );
            observation.observe(&context, &target, true);
            let first = observation.take_metadata(now).unwrap();
            assert!(
                observation.take_metadata(now + METADATA_PERIOD).is_none(),
                "one flight"
            );
            let prepared = PreparedInspection {
                target: target.clone(),
                dependencies: Dependencies {
                    thread: "scope".into(),
                    parent_thread: None,
                },
                value: json!({"observed":true}),
                observed_at: "fixture".into(),
                limited_fields: vec![],
            };
            assert!(matches!(
                observation.complete_metadata(&first, 1, Ok(prepared)),
                Completion::Updated
            ));
            assert!(
                observation.take_metadata(now + METADATA_PERIOD).is_none(),
                "quiet has zero jobs"
            );
            observation.changed("scope");
            let pending = observation.take_metadata(now + METADATA_PERIOD).unwrap();
            observation.observe(&context, &target, false);
            assert!(
                observation
                    .take_metadata(now + METADATA_PERIOD * 2)
                    .is_none(),
                "hidden after dirty"
            );
            assert!(matches!(
                observation.complete_metadata(
                    &pending,
                    2,
                    Err(ApiError::new(ErrorCode::InvalidState, "late"))
                ),
                Completion::Retired
            ));
            observation.close();
            assert!(
                observation
                    .take_metadata(now + METADATA_PERIOD * 3)
                    .is_none()
            );
        }
    }
}
