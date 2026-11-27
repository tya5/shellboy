use super::{
    MutationResult, Result, Store, error, execute_kind, inspection_work, open_inspection,
    sql_error, target,
};
use crate::protocol::{ErrorCode, Origin, Request, StatusArgs, UsageGetArgs};
use rusqlite::{OptionalExtension, Transaction, params};

impl Store {
    pub(super) async fn inspect_status(
        &self,
        request: &Request,
        caller: Option<Origin>,
        mut args: StatusArgs,
    ) -> Result<MutationResult> {
        if args.thread_id.is_none() && args.run_id.is_none() {
            args.thread_id = caller.as_ref().map(|origin| origin.thread_id.clone());
        }
        let instance = self.instance_id.clone();
        let epoch = self.operation_epoch.current();
        let path = self.database.path().to_path_buf();
        let observer = self.database.clone();
        let kind = format!("{:?}", execute_kind::Kind::of(&request.command));
        let queued_at = std::time::Instant::now();
        let requester = Some(if caller.is_some() {
            "shell"
        } else {
            "external"
        });
        inspection_work(move || {
            let mut db = open_inspection(&path)?;
            observer.observe_inspection(&mut db, &kind, queued_at, requester, |db| {
                let tx = db.transaction().map_err(sql_error)?;
                let result = crate::status::snapshot(&tx, &args, &instance, &epoch)?;
                tx.commit().map_err(sql_error)?;
                MutationResult::value(result)
            })
        })
        .await?
    }

    pub(super) fn usage_get(
        &self,
        tx: &Transaction<'_>,
        args: &UsageGetArgs,
        caller: Option<&Origin>,
    ) -> Result<MutationResult> {
        let mut args = args.clone();
        if let Some(node_id) = &args.since_node_id {
            let scope_thread = if let Some(turn) = &args.turn_id {
                tx.query_row("SELECT thread_id FROM turn WHERE id=?1", [turn], |r| {
                    r.get::<_, String>(0)
                })
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| error(ErrorCode::TargetUnavailable, "Usage scope is unavailable"))?
            } else if let Some(run) = &args.run_id {
                tx.query_row("SELECT thread_id FROM run WHERE id=?1", [run], |r| {
                    r.get::<_, String>(0)
                })
                .optional()
                .map_err(sql_error)?
                .ok_or_else(|| error(ErrorCode::TargetUnavailable, "Usage scope is unavailable"))?
            } else {
                target(args.thread_id.as_deref(), caller)?
            };
            let at: String = tx.query_row("SELECT n.created_at FROM node n JOIN thread_path p ON p.node_id=n.id AND p.thread_id=?2 WHERE n.id=?1 AND n.deleted_reason IS NULL", params![node_id,scope_thread], |r| r.get(0)).optional().map_err(sql_error)?
                .ok_or_else(|| error(ErrorCode::TargetUnavailable, "Usage start node is unavailable on the selected Thread path"))?;
            args.since = Some(at.into_boxed_str());
            args.since_node_id = None;
            // Grouped requests otherwise default to every Thread; anchor them to this path.
            if args.group_by.is_some()
                && args.thread_id.is_none()
                && args.run_id.is_none()
                && args.turn_id.is_none()
            {
                args.thread_id = Some(scope_thread.into_boxed_str());
            }
        }
        if let Some(attempt) = &args.attempt_id {
            return MutationResult::value(crate::usage_totals::read_attempt(tx, attempt)?);
        }
        if args.group_by.is_some() {
            return MutationResult::value(crate::usage_totals::breakdown::read(
                tx,
                &args,
                &self.instance_id,
            )?);
        }
        let scope = if let Some(id) = &args.turn_id {
            crate::usage_totals::Scope::Turn(id.clone())
        } else if let Some(id) = &args.run_id {
            crate::usage_totals::Scope::Run(id.clone())
        } else {
            crate::usage_totals::Scope::Thread {
                id: target(args.thread_id.as_deref(), caller)?,
                children: args.include_children,
            }
        };
        MutationResult::value(crate::usage_totals::read_period_logged(
            tx,
            &scope,
            self.usage_context,
            self.usage_diagnostics.as_ref(),
            &crate::readback::Window::new(args.since.as_deref(), args.until.as_deref())?,
        )?)
    }
}
