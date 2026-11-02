//! Messaging, handoffs, shared context, decisions and approvals (impl Engine).

use crate::engine::{Actor, Emit, Engine};
use chrono::Utc;
use maw_core::*;
use maw_storage::Db;
use serde_json::json;

pub struct NewMessage {
    pub recipient: AgentId,
    pub message_type: MessageType,
    pub subject: String,
    pub content: String,
    pub task_id: Option<TaskId>,
    pub correlation_id: Option<String>,
    pub idempotency_key: Option<String>,
}

const MAX_MESSAGE_BYTES: usize = 32 * 1024;

impl Engine {
    fn display_name(&self, db: &Db, id: &AgentId) -> String {
        match id.as_str() {
            "user" => "user".into(),
            "orchestrator" => "orchestrator".into(),
            _ => db
                .get_agent(id)
                .ok()
                .flatten()
                .map(|a| a.name)
                .unwrap_or_else(|| id.0.clone()),
        }
    }

    /// Persist a message (idempotent per sender+key). Creating a message does NOT imply delivery;
    /// the supervisor delivers it to the recipient's session and records `delivered_at`.
    pub(crate) fn send_message_tx(
        &self,
        db: &Db,
        emit: &mut Emit,
        sender: &AgentId,
        sender_name: &str,
        m: NewMessage,
    ) -> CoreResult<Message> {
        if m.content.len() > MAX_MESSAGE_BYTES {
            return Err(CoreError::LimitExceeded(format!(
                "message larger than {MAX_MESSAGE_BYTES} bytes"
            )));
        }
        if m.subject.trim().is_empty() && m.content.trim().is_empty() {
            return Err(CoreError::InvalidInput("message is empty".into()));
        }
        if let Some(key) = &m.idempotency_key {
            if let Some(existing) = db.find_message_by_idempotency(&self.project.id, sender, key)? {
                return Ok(existing);
            }
        }
        let recipient = db
            .get_agent(&m.recipient)?
            .ok_or_else(|| CoreError::not_found("agent", &m.recipient))?;
        let msg = Message {
            id: MessageId::new(),
            project_id: self.project.id.clone(),
            sender_agent_id: sender.clone(),
            recipient_agent_id: m.recipient.clone(),
            task_id: m.task_id.clone(),
            message_type: m.message_type,
            subject: m.subject,
            content: maw_core::redact::redact(&m.content),
            created_at: Utc::now(),
            delivered_at: None,
            acknowledged_at: None,
            correlation_id: m.correlation_id,
        };
        db.insert_message(&msg, m.idempotency_key.as_deref())?;
        let task_key = match &msg.task_id {
            Some(t) => db.get_task(t)?.map(|t| t.key),
            None => None,
        };
        let mut ev = json!({
            "message_id": msg.id, "from": sender_name, "from_id": sender, "to": recipient.name, "to_id": recipient.id,
            "message_type": msg.message_type.as_str(), "subject": msg.subject, "task_key": task_key
        });
        if msg.message_type == MessageType::Handoff {
            ev["handoff"] = json!(true);
        }
        emit.push(
            EventKind::MessageSent,
            sender_agent(sender).as_ref(),
            msg.task_id.as_ref(),
            ev,
        );
        Ok(msg)
    }

    /// Notify the Leader about something (no-op when the actor is the Leader or none exists).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn notify_leader_tx(
        &self,
        db: &Db,
        emit: &mut Emit,
        actor: &Actor,
        task: Option<&Task>,
        mt: MessageType,
        subject: &str,
        content: &str,
    ) -> CoreResult<()> {
        let Some(leader) = db
            .list_agents(&self.project.id)?
            .into_iter()
            .find(|a| a.role == Role::Leader)
        else {
            return Ok(());
        };
        if actor.agent_id().as_ref() == Some(&leader.id) {
            return Ok(());
        }
        let sender = actor.id();
        let name = self.display_name(db, &sender);
        self.send_message_tx(
            db,
            emit,
            &sender,
            &name,
            NewMessage {
                recipient: leader.id,
                message_type: mt,
                subject: subject.to_string(),
                content: content.to_string(),
                task_id: task.map(|t| t.id.clone()),
                correlation_id: task.map(|t| t.id.0.clone()),
                idempotency_key: None,
            },
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send_message(
        &self,
        actor: &Actor,
        to: &str,
        mt: MessageType,
        subject: &str,
        content: &str,
        task: Option<&str>,
        idem: Option<String>,
    ) -> CoreResult<Message> {
        self.tx(|db, emit| {
            let recipient = if to == "leader" {
                db.list_agents(&self.project.id)?
                    .into_iter()
                    .find(|a| a.role == Role::Leader)
                    .ok_or_else(|| CoreError::not_found("agent", "leader"))?
            } else {
                db.list_agents(&self.project.id)?
                    .into_iter()
                    .find(|a| a.id.0 == to || a.name.eq_ignore_ascii_case(to))
                    .ok_or_else(|| CoreError::not_found("agent", to))?
            };
            if actor.agent_id().as_ref() == Some(&recipient.id) {
                return Err(CoreError::InvalidInput("cannot message yourself".into()));
            }
            // Only the Leader / user may give INSTRUCTIONs; workers use QUESTION/STATUS/etc.
            if mt == MessageType::Instruction && !actor.is_leader() {
                return Err(CoreError::Forbidden(
                    "only the Leader can send INSTRUCTION messages".into(),
                ));
            }
            let task_id = match task {
                Some(t) => Some(self.task_ref(db, t)?.id),
                None => None,
            };
            let name = self.display_name(db, &actor.id());
            self.send_message_tx(
                db,
                emit,
                &actor.id(),
                &name,
                NewMessage {
                    recipient: recipient.id,
                    message_type: mt,
                    subject: subject.to_string(),
                    content: content.to_string(),
                    task_id: task_id.clone(),
                    correlation_id: task_id.map(|t| t.0),
                    idempotency_key: idem,
                },
            )
        })
    }

    pub fn inbox(
        &self,
        actor: &Actor,
        unacked_only: bool,
        limit: usize,
    ) -> CoreResult<Vec<Message>> {
        let id = actor
            .agent_id()
            .ok_or_else(|| CoreError::Forbidden("only agents have an inbox".into()))?;
        self.read(|db| db.inbox(&id, unacked_only, limit.clamp(1, 200)))
    }

    pub fn ack_message(&self, actor: &Actor, message_id: &str) -> CoreResult<Message> {
        let id = actor
            .agent_id()
            .ok_or_else(|| CoreError::Forbidden("only agents acknowledge messages".into()))?;
        self.tx(|db, emit| {
            let m = db.get_message(&MessageId::from(message_id))?.ok_or_else(|| CoreError::not_found("message", message_id))?;
            if m.recipient_agent_id != id {
                return Err(CoreError::Forbidden("you can only acknowledge your own messages".into()));
            }
            if m.acknowledged_at.is_some() {
                return Ok(m);
            }
            db.mark_acked(&m.id)?;
            emit.push(EventKind::MessageAcked, Some(&id), m.task_id.as_ref(), json!({ "message_id": m.id, "from_id": m.sender_agent_id, "by": self.display_name(db, &id) }));
            db.get_message(&m.id)?.ok_or_else(|| CoreError::Internal("message vanished".into()))
        })
    }

    pub fn mark_delivered(&self, message: &Message) -> CoreResult<()> {
        self.tx(|db, emit| {
            db.mark_delivered(&message.id)?;
            emit.push(
                EventKind::MessageDelivered,
                Some(&message.recipient_agent_id),
                message.task_id.as_ref(),
                json!({ "message_id": message.id, "to_id": message.recipient_agent_id, "to": self.display_name(db, &message.recipient_agent_id), "from_id": message.sender_agent_id, "from": self.display_name(db, &message.sender_agent_id) }),
            );
            Ok(())
        })
    }

    pub fn undelivered_for(&self, agent: &AgentId) -> CoreResult<Vec<Message>> {
        self.read(|db| db.undelivered(agent))
    }

    pub fn agents_with_pending_messages(&self) -> CoreResult<Vec<AgentId>> {
        self.read(|db| db.agents_with_undelivered(&self.project.id))
    }

    // ------------------------------------------------------------------ handoffs

    /// Owner hands a task to another agent with context. Ownership moves only on acceptance.
    pub fn create_handoff(
        &self,
        actor: &Actor,
        task: &str,
        to: &str,
        summary: &str,
        context: &str,
    ) -> CoreResult<Message> {
        if summary.trim().is_empty() {
            return Err(CoreError::InvalidInput("a handoff needs a summary".into()));
        }
        self.tx(|db, emit| {
            let t = self.task_ref(db, task)?;
            match actor {
                Actor::Agent { id, .. } if t.owner_agent_id.as_ref() == Some(id) => {}
                _ if actor.is_leader() => {}
                _ => return Err(CoreError::Forbidden("only the task owner or the Leader can hand off a task".into())),
            }
            if t.status.is_terminal() {
                return Err(CoreError::Precondition(format!("{} is {}", t.key, t.status)));
            }
            let target = db
                .list_agents(&self.project.id)?
                .into_iter()
                .find(|a| a.id.0 == to || a.name.eq_ignore_ascii_case(to))
                .ok_or_else(|| CoreError::not_found("agent", to))?;
            if !target.role.is_executor() {
                return Err(CoreError::InvalidInput(format!("{} cannot take over implementation tasks", target.name)));
            }
            if Some(&target.id) == t.owner_agent_id.as_ref() {
                return Err(CoreError::InvalidInput("task is already owned by that agent".into()));
            }
            let name = self.display_name(db, &actor.id());
            let m = self.send_message_tx(
                db,
                emit,
                &actor.id(),
                &name,
                NewMessage {
                    recipient: target.id.clone(),
                    message_type: MessageType::Handoff,
                    subject: format!("Handoff of {}: {}", t.key, t.title),
                    content: format!("{summary}\n\n--- context ---\n{context}"),
                    task_id: Some(t.id.clone()),
                    correlation_id: Some(t.id.0.clone()),
                    idempotency_key: None,
                },
            )?;
            emit.push(EventKind::HandoffCreated, actor.agent_id().as_ref(), Some(&t.id), json!({ "message_id": m.id, "task_key": t.key, "from": name, "from_id": actor.id(), "to": target.name, "to_id": target.id }));
            Ok(m)
        })
    }

    pub fn accept_handoff(&self, actor: &Actor, message_id: &str) -> CoreResult<Task> {
        let me = actor
            .agent_id()
            .ok_or_else(|| CoreError::Forbidden("only agents accept handoffs".into()))?;
        self.tx(|db, emit| {
            let m = db.get_message(&MessageId::from(message_id))?.ok_or_else(|| CoreError::not_found("message", message_id))?;
            if m.message_type != MessageType::Handoff || m.recipient_agent_id != me {
                return Err(CoreError::Forbidden("not a handoff addressed to you".into()));
            }
            let tid = m.task_id.clone().ok_or_else(|| CoreError::Internal("handoff without task".into()))?;
            let mut t = db.get_task(&tid)?.ok_or_else(|| CoreError::not_found("task", &tid))?;
            if t.owner_agent_id.as_ref() == Some(&me) {
                return Ok(t);
            }
            if t.status.is_terminal() {
                return Err(CoreError::Precondition(format!("{} is {}", t.key, t.status)));
            }
            let prev = t.owner_agent_id.replace(me.clone());
            if let Some(p) = &prev {
                if let Some(mut pa) = db.get_agent(p)? {
                    if pa.current_task_id.as_ref() == Some(&t.id) {
                        pa.current_task_id = None;
                        db.update_agent(&pa)?;
                    }
                }
            }
            db.mark_acked(&m.id)?;
            db.update_task(&t)?;
            if matches!(t.status, TaskStatus::Running | TaskStatus::Blocked) {
                // new owner starts a fresh attempt; the previous attempt is closed explicitly.
                db.end_attempts(&t.id, "handed_off", None)?;
                self.transition(emit, db, &mut t, TaskStatus::Ready, EventKind::TaskReleased, json!({ "reason": "handoff" }))?;
            }
            if t.status == TaskStatus::Ready {
                let name = self.display_name(db, &me);
                self.transition(emit, db, &mut t, TaskStatus::Assigned, EventKind::TaskAssigned, json!({ "to": name, "to_id": me, "by": "handoff", "by_id": m.sender_agent_id }))?;
            }
            emit.push(
                EventKind::HandoffAccepted,
                Some(&me),
                Some(&t.id),
                json!({ "message_id": m.id, "task_key": t.key, "from_id": m.sender_agent_id, "from": self.display_name(db, &m.sender_agent_id), "to": self.display_name(db, &me), "to_id": me }),
            );
            db.get_task(&t.id)?.ok_or_else(|| CoreError::Internal("task vanished".into()))
        })
    }

    // ------------------------------------------------------------------ help

    /// Escalate to the Leader with a structured request.
    pub fn request_help(
        &self,
        actor: &Actor,
        task: Option<&str>,
        topic: &str,
        details: &str,
    ) -> CoreResult<Message> {
        let aid = actor
            .agent_id()
            .ok_or_else(|| CoreError::Forbidden("only agents request help".into()))?;
        self.tx(|db, emit| {
            let leader = db.list_agents(&self.project.id)?.into_iter().find(|a| a.role == Role::Leader).ok_or_else(|| CoreError::Precondition("no Leader to ask".into()))?;
            if leader.id == aid {
                return Err(CoreError::InvalidInput("the Leader cannot request help from itself".into()));
            }
            let t = match task {
                Some(t) => Some(self.task_ref(db, t)?),
                None => None,
            };
            let name = self.display_name(db, &aid);
            emit.push(EventKind::HelpRequested, Some(&aid), t.as_ref().map(|t| &t.id), json!({ "from": name, "from_id": aid, "to_id": leader.id, "topic": topic, "task_key": t.as_ref().map(|t| t.key.clone()) }));
            self.send_message_tx(
                db,
                emit,
                &aid,
                &name,
                NewMessage {
                    recipient: leader.id,
                    message_type: MessageType::Question,
                    subject: format!("Help requested: {topic}"),
                    content: details.to_string(),
                    task_id: t.map(|t| t.id),
                    correlation_id: None,
                    idempotency_key: None,
                },
            )
        })
    }

    // ------------------------------------------------------------------ shared context

    pub fn publish_context(
        &self,
        actor: &Actor,
        kind: &str,
        key: &str,
        content: &str,
        tags: Vec<String>,
        task: Option<&str>,
    ) -> CoreResult<ContextEntry> {
        if key.trim().is_empty() || content.trim().is_empty() {
            return Err(CoreError::InvalidInput(
                "key and content are required".into(),
            ));
        }
        if content.len() > 64 * 1024 {
            return Err(CoreError::LimitExceeded(
                "context entry larger than 64KiB".into(),
            ));
        }
        self.tx(|db, emit| {
            let task_id = match task {
                Some(t) => Some(self.task_ref(db, t)?.id),
                None => None,
            };
            let c = ContextEntry {
                id: format!("ctx_{}", uuid_simple()),
                project_id: self.project.id.clone(),
                kind: kind.to_string(),
                key: key.to_string(),
                content: maw_core::redact::redact(content),
                tags,
                task_id,
                author: actor.id(),
                created_at: Utc::now(),
            };
            db.add_context(&c)?;
            let ev = if kind == "decision" { EventKind::DecisionRecorded } else { EventKind::ContextPublished };
            emit.push(ev, actor.agent_id().as_ref(), c.task_id.as_ref(), json!({ "id": c.id, "key": c.key, "kind": c.kind, "by": self.display_name(db, &actor.id()), "by_id": actor.id() }));
            Ok(c)
        })
    }

    pub fn search_context(
        &self,
        kind: Option<&str>,
        query: Option<&str>,
        limit: usize,
    ) -> CoreResult<Vec<ContextEntry>> {
        self.read(|db| db.search_context(&self.project.id, kind, query, limit.clamp(1, 100)))
    }

    // ------------------------------------------------------------------ approvals

    /// Persist a provider approval request so it survives restarts and shows in the Approval Center.
    pub fn record_approval(
        &self,
        agent: &AgentId,
        kind: &str,
        summary: &str,
        reason: Option<String>,
        details: serde_json::Value,
        request_ref: &str,
    ) -> CoreResult<Approval> {
        self.tx(|db, emit| {
            let a = db.get_agent(agent)?.ok_or_else(|| CoreError::not_found("agent", agent))?;
            let ap = Approval {
                id: ApprovalId::new(),
                project_id: self.project.id.clone(),
                agent_id: agent.clone(),
                task_id: a.current_task_id.clone(),
                kind: kind.to_string(),
                summary: maw_core::redact::redact(summary),
                reason,
                details,
                request_ref: request_ref.to_string(),
                status: ApprovalStatus::Pending,
                requested_at: Utc::now(),
                resolved_at: None,
            };
            db.insert_approval(&ap)?;
            emit.push(EventKind::PermissionRequested, Some(agent), ap.task_id.as_ref(), json!({ "approval_id": ap.id, "kind": ap.kind, "summary": ap.summary, "agent": a.name }));
            Ok(ap)
        })
    }

    pub fn resolve_approval(
        &self,
        id: &ApprovalId,
        status: ApprovalStatus,
    ) -> CoreResult<Approval> {
        self.tx(|db, emit| {
            let ap = db
                .get_approval(id)?
                .ok_or_else(|| CoreError::not_found("approval", id))?;
            if ap.status != ApprovalStatus::Pending {
                return Err(CoreError::Conflict(format!(
                    "approval already {}",
                    ap.status.as_str()
                )));
            }
            db.resolve_approval(id, status)?;
            emit.push(
                EventKind::PermissionResolved,
                Some(&ap.agent_id),
                ap.task_id.as_ref(),
                json!({ "approval_id": id, "status": status.as_str(), "summary": ap.summary }),
            );
            db.get_approval(id)?
                .ok_or_else(|| CoreError::Internal("approval vanished".into()))
        })
    }

    pub fn pending_approvals(&self) -> CoreResult<Vec<Approval>> {
        self.read(|db| db.list_approvals(&self.project.id, true))
    }

    pub fn expire_approvals_for(&self, agent: &AgentId) {
        let _ = self
            .store
            .write(|db| db.expire_agent_approvals(agent).map(|_| ()));
    }
}

fn sender_agent(id: &AgentId) -> Option<AgentId> {
    match id.as_str() {
        "user" | "orchestrator" => None,
        _ => Some(id.clone()),
    }
}

fn uuid_simple() -> String {
    use rand::RngCore;
    let mut b = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}
