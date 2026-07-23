use crate::backend::TurnInput;
use crate::state::{AgentState, ClientIdentity};
use remagic_core::AppId;
use remagic_protocol::{
    AgentClientMessage, AgentErrorCode, AgentEvent, AgentHistoryTurn, AgentLane, AgentProfile,
    AgentToolDefinition, AGENT_PROTOCOL_V1,
};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

pub(crate) type ConnectionTurn = Mutex<Option<(AppId, String)>>;

struct StartArguments {
    request_id: String,
    app_id: AppId,
    profile: AgentProfile,
    lane: AgentLane,
    system_prompt: String,
    input: String,
    history: Vec<AgentHistoryTurn>,
    tools: Vec<AgentToolDefinition>,
}

pub(crate) async fn dispatch(
    message: AgentClientMessage,
    state: &AgentState,
    identity: &ClientIdentity,
    events: &mpsc::Sender<AgentEvent>,
    connection_turn: &Arc<ConnectionTurn>,
) {
    match message {
        AgentClientMessage::Status {
            request_id, app_id, ..
        } => send_status(state, events, request_id, app_id).await,
        AgentClientMessage::StartTurn {
            request_id,
            app_id,
            profile,
            lane,
            system_prompt,
            input,
            history,
            tools,
            ..
        } => {
            start_turn(
                state,
                identity,
                events,
                connection_turn,
                StartArguments {
                    request_id,
                    app_id,
                    profile,
                    lane,
                    system_prompt,
                    input,
                    history,
                    tools,
                },
            )
            .await
        }
        AgentClientMessage::CancelTurn {
            request_id,
            app_id,
            turn_id,
            ..
        } => cancel_turn(state, events, request_id, app_id, turn_id).await,
        AgentClientMessage::ToolResult {
            request_id,
            app_id,
            turn_id,
            ..
        } => {
            send_error(
                events,
                &request_id,
                &app_id,
                Some(turn_id),
                AgentErrorCode::ToolNotPending,
                "the safe Pi backend has no pending platform tool",
                false,
            )
            .await
        }
        AgentClientMessage::ReloadProfile {
            request_id,
            app_id,
            profile,
            ..
        } => reload_profile(state, events, request_id, app_id, profile).await,
        AgentClientMessage::NewSession {
            request_id, app_id, ..
        } => new_session(state, events, request_id, app_id).await,
    }
}

async fn start_turn(
    state: &AgentState,
    identity: &ClientIdentity,
    events: &mpsc::Sender<AgentEvent>,
    connection_turn: &Arc<ConnectionTurn>,
    request: StartArguments,
) {
    if !request.tools.is_empty() {
        send_error(
            events,
            &request.request_id,
            &request.app_id,
            None,
            AgentErrorCode::InvalidRequest,
            "platform tool definitions are unavailable in this build",
            false,
        )
        .await;
        return;
    }
    let started = match state
        .start(
            &request.app_id,
            &request.request_id,
            request.profile.clone(),
            &request.system_prompt,
            request.lane,
            &identity.principal,
        )
        .await
    {
        Ok(started) => started,
        Err(code) => {
            let message = match code {
                AgentErrorCode::Unavailable => "Pi runtime is not installed",
                AgentErrorCode::Busy => "this application already has an active turn",
                _ => "turn could not start",
            };
            send_error(
                events,
                &request.request_id,
                &request.app_id,
                None,
                code,
                message,
                true,
            )
            .await;
            return;
        }
    };
    let turn_id = started.id.clone();
    *connection_turn.lock().await = Some((request.app_id.clone(), turn_id.clone()));
    let _ = events
        .send(AgentEvent::Accepted {
            protocol: AGENT_PROTOCOL_V1,
            request_id: request.request_id.clone(),
            app_id: request.app_id.clone(),
            turn_id: turn_id.clone(),
        })
        .await;
    spawn_turn(state, events, connection_turn, request, turn_id, started);
}

fn spawn_turn(
    state: &AgentState,
    events: &mpsc::Sender<AgentEvent>,
    connection_turn: &Arc<ConnectionTurn>,
    request: StartArguments,
    turn_id: String,
    started: crate::state::StartedTurn,
) {
    let state = state.clone();
    let events = events.clone();
    let owned_turn = Arc::clone(connection_turn);
    tokio::spawn(async move {
        let input = TurnInput {
            request_id: request.request_id.clone(),
            app_id: request.app_id.clone(),
            turn_id: turn_id.clone(),
            input: request.input,
            history: request.history,
        };
        if let Err(message) = started
            .worker
            .turn(input, started.cancel, events.clone())
            .await
        {
            send_error(
                &events,
                &request.request_id,
                &request.app_id,
                Some(turn_id.clone()),
                AgentErrorCode::BackendFailed,
                message,
                true,
            )
            .await;
        }
        state.finish(&request.app_id, &turn_id).await;
        clear_owned_turn(&owned_turn, &request.app_id, &turn_id).await;
    });
}

async fn clear_owned_turn(owned: &ConnectionTurn, app_id: &AppId, turn_id: &str) {
    let mut current = owned.lock().await;
    if current
        .as_ref()
        .is_some_and(|(app, turn)| app == app_id && turn == turn_id)
    {
        *current = None;
    }
}

async fn cancel_turn(
    state: &AgentState,
    events: &mpsc::Sender<AgentEvent>,
    request_id: String,
    app_id: AppId,
    turn_id: String,
) {
    if !state.cancel(&app_id, &turn_id).await {
        send_error(
            events,
            &request_id,
            &app_id,
            Some(turn_id),
            AgentErrorCode::TurnNotFound,
            "turn is not active",
            false,
        )
        .await;
    }
}

async fn reload_profile(
    state: &AgentState,
    events: &mpsc::Sender<AgentEvent>,
    request_id: String,
    app_id: AppId,
    profile: Option<AgentProfile>,
) {
    match state.reload_profile(&app_id, profile).await {
        Ok(()) => send_status(state, events, request_id, app_id).await,
        Err(code) => {
            send_error(
                events,
                &request_id,
                &app_id,
                None,
                code,
                "profile cannot change during an active turn",
                true,
            )
            .await
        }
    }
}

async fn new_session(
    state: &AgentState,
    events: &mpsc::Sender<AgentEvent>,
    request_id: String,
    app_id: AppId,
) {
    match state.new_session(&app_id).await {
        Ok(()) => send_status(state, events, request_id, app_id).await,
        Err(code) => {
            send_error(
                events,
                &request_id,
                &app_id,
                None,
                code,
                "Pi session could not be reset",
                true,
            )
            .await
        }
    }
}

async fn send_status(
    state: &AgentState,
    events: &mpsc::Sender<AgentEvent>,
    request_id: String,
    app_id: AppId,
) {
    let _ = events
        .send(AgentEvent::Status {
            protocol: AGENT_PROTOCOL_V1,
            request_id,
            app_id: app_id.clone(),
            status: Box::new(state.status(&app_id).await),
        })
        .await;
}

pub(crate) async fn cancel_connection_turn(
    state: &AgentState,
    connection_turn: &ConnectionTurn,
) -> bool {
    let Some((app_id, turn_id)) = connection_turn.lock().await.take() else {
        return false;
    };
    state.cancel(&app_id, &turn_id).await
}

pub(crate) async fn send_error(
    events: &mpsc::Sender<AgentEvent>,
    request_id: &str,
    app_id: &AppId,
    turn_id: Option<String>,
    code: AgentErrorCode,
    message: impl Into<String>,
    retryable: bool,
) {
    let _ = events
        .send(AgentEvent::Error {
            protocol: AGENT_PROTOCOL_V1,
            request_id: request_id.into(),
            app_id: app_id.clone(),
            turn_id,
            code,
            message: message.into(),
            retryable,
        })
        .await;
}
