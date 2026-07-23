use remagic_core::AppId;
#[cfg(test)]
use remagic_protocol::AgentErrorCode;
use remagic_protocol::{AgentEvent, AgentHistoryTurn, AgentProfile};
use serde_json::{json, Value};
use std::path::Path;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, watch};

mod config;
use config::configured_command;
pub(crate) use config::provider_configured;
mod events;
use events::*;
mod wire;
use wire::next_rpc_line;

const RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const ABORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const SPAWN_BUSY_RETRIES: usize = 3;

pub(crate) struct TurnInput {
    pub request_id: String,
    pub app_id: AppId,
    pub turn_id: String,
    pub input: String,
    pub history: Vec<AgentHistoryTurn>,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkerHandle {
    commands: mpsc::Sender<WorkerCommand>,
}

enum WorkerCommand {
    Turn {
        input: TurnInput,
        cancel: watch::Receiver<bool>,
        events: mpsc::Sender<AgentEvent>,
        done: oneshot::Sender<Result<(), String>>,
    },
    NewSession {
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown,
}

struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    command_sequence: u64,
    hydrate_history_on_next: bool,
}

impl WorkerHandle {
    pub async fn spawn(
        pi_binary: &Path,
        app_id: &AppId,
        profile: &AgentProfile,
        system_prompt: &str,
    ) -> Result<Self, String> {
        let worker = Worker::spawn(pi_binary, app_id, profile, system_prompt)?;
        let (commands, receiver) = mpsc::channel(8);
        tokio::spawn(worker.run(receiver));
        Ok(Self { commands })
    }

    pub fn is_closed(&self) -> bool {
        self.commands.is_closed()
    }

    #[cfg(test)]
    pub(crate) fn same_worker(&self, other: &Self) -> bool {
        self.commands.same_channel(&other.commands)
    }

    pub async fn turn(
        &self,
        input: TurnInput,
        cancel: watch::Receiver<bool>,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<(), String> {
        let (done, completed) = oneshot::channel();
        self.commands
            .send(WorkerCommand::Turn {
                input,
                cancel,
                events,
                done,
            })
            .await
            .map_err(|_| "Pi worker exited".to_owned())?;
        completed.await.map_err(|_| "Pi worker exited".to_owned())?
    }

    pub async fn new_session(&self) -> Result<(), String> {
        let (reply, result) = oneshot::channel();
        self.commands
            .send(WorkerCommand::NewSession { reply })
            .await
            .map_err(|_| "Pi worker exited".to_owned())?;
        result.await.map_err(|_| "Pi worker exited".to_owned())?
    }

    pub async fn shutdown(&self) {
        let _ = self.commands.send(WorkerCommand::Shutdown).await;
    }
}

impl Worker {
    fn spawn(
        pi_binary: &Path,
        app_id: &AppId,
        profile: &AgentProfile,
        system_prompt: &str,
    ) -> Result<Self, String> {
        let mut command = configured_command(pi_binary, app_id, profile, system_prompt)?;
        let mut child = spawn_with_busy_retry(&mut command)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Pi stdin unavailable".to_owned())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Pi stdout unavailable".to_owned())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            command_sequence: 0,
            hydrate_history_on_next: true,
        })
    }

    async fn run(mut self, mut commands: mpsc::Receiver<WorkerCommand>) {
        while let Some(command) = commands.recv().await {
            let keep_running = match command {
                WorkerCommand::Turn {
                    input,
                    cancel,
                    events,
                    done,
                } => {
                    let keep_running = self.run_turn(input, cancel, events).await;
                    let _ = done.send(Ok(()));
                    keep_running
                }
                WorkerCommand::NewSession { reply } => {
                    let result = self.new_session().await;
                    let success = result.is_ok();
                    let _ = reply.send(result);
                    success
                }
                WorkerCommand::Shutdown => false,
            };
            if !keep_running {
                break;
            }
        }
        let _ = self.child.kill().await;
    }

    async fn run_turn(
        &mut self,
        turn: TurnInput,
        mut cancel: watch::Receiver<bool>,
        events: mpsc::Sender<AgentEvent>,
    ) -> bool {
        let prompt = compose_prompt(&turn, self.hydrate_history_on_next);
        let command = json!({"id": turn.turn_id, "type": "prompt", "message": prompt});
        if let Err(error) = self.write_command(&command).await {
            send_error(&events, &turn, error, true).await;
            return false;
        }
        let mut accepted = false;
        let mut published = String::new();
        loop {
            tokio::select! {
                changed = cancel.changed() => {
                    if changed.is_ok() && *cancel.borrow() {
                        return self.abort_turn(&turn, &events).await;
                    }
                }
                line = next_rpc_line(&mut self.stdout) => match line {
                    Ok(Some(line)) => {
                        match process_line(&line, &turn, &events, &mut published, &mut accepted).await {
                            LineOutcome::Continue => {}
                            LineOutcome::Complete => {
                                self.command_sequence += 1;
                                self.hydrate_history_on_next = false;
                                return true;
                            }
                            LineOutcome::Rejected(message) => {
                                send_error(&events, &turn, message, false).await;
                                return true;
                            }
                            LineOutcome::Fatal(message) => {
                                send_error(&events, &turn, message, false).await;
                                return false;
                            }
                        }
                    }
                    Ok(None) => {
                        send_error(&events, &turn, "Pi exited before agent_end", true).await;
                        return false;
                    }
                    Err(error) => {
                        send_error(&events, &turn, error.to_string(), true).await;
                        return false;
                    }
                }
            }
        }
    }

    async fn abort_turn(&mut self, turn: &TurnInput, events: &mpsc::Sender<AgentEvent>) -> bool {
        let abort_id = format!("abort:{}", turn.turn_id);
        if self
            .write_command(&json!({"id": abort_id, "type": "abort"}))
            .await
            .is_err()
        {
            send_cancelled(events, turn).await;
            return false;
        }
        let drained = tokio::time::timeout(ABORT_TIMEOUT, async {
            let mut response_ok = false;
            let mut ended = false;
            while !response_ok || !ended {
                let Some(line) = next_rpc_line(&mut self.stdout)
                    .await
                    .map_err(|e| e.to_string())?
                else {
                    return Err("Pi exited during abort".to_owned());
                };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if response_matches(&value, &abort_id, "abort") {
                    response_ok = value.get("success").and_then(Value::as_bool) == Some(true);
                }
                ended |= value.get("type").and_then(Value::as_str) == Some("agent_end");
            }
            Ok::<_, String>(())
        })
        .await;
        send_cancelled(events, turn).await;
        matches!(drained, Ok(Ok(())))
    }

    async fn new_session(&mut self) -> Result<(), String> {
        let id = format!("new-session:{}", self.command_sequence);
        self.write_command(&json!({"id": id, "type": "new_session"}))
            .await?;
        self.wait_response(&id, "new_session").await?;
        self.command_sequence += 1;
        self.hydrate_history_on_next = false;
        Ok(())
    }

    async fn wait_response(&mut self, id: &str, command: &str) -> Result<(), String> {
        tokio::time::timeout(RPC_TIMEOUT, async {
            loop {
                let Some(line) = next_rpc_line(&mut self.stdout)
                    .await
                    .map_err(|e| e.to_string())?
                else {
                    return Err("Pi exited while awaiting RPC response".to_owned());
                };
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if response_matches(&value, id, command) {
                    return (value.get("success").and_then(Value::as_bool) == Some(true))
                        .then_some(())
                        .ok_or_else(|| rpc_error(&value));
                }
            }
        })
        .await
        .map_err(|_| format!("Pi {command} response timed out"))?
    }

    async fn write_command(&mut self, value: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        self.stdin
            .write_all(&bytes)
            .await
            .map_err(|error| error.to_string())?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|error| error.to_string())?;
        self.stdin.flush().await.map_err(|error| error.to_string())
    }
}

fn spawn_with_busy_retry(command: &mut tokio::process::Command) -> Result<Child, String> {
    for attempt in 0..=SPAWN_BUSY_RETRIES {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error)
                if error.raw_os_error() == Some(libc::ETXTBSY) && attempt < SPAWN_BUSY_RETRIES =>
            {
                // An atomic runtime update or delayed filesystem close can
                // briefly leave the executable busy. Retry only this precise
                // kernel condition, for a bounded 30 ms total; every other
                // startup failure remains immediately visible.
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Pi executable remained busy after bounded retries".into())
}

fn compose_prompt(turn: &TurnInput, include_history: bool) -> String {
    let mut result = String::new();
    if include_history {
        for history in &turn.history {
            result.push_str("User: ");
            result.push_str(&history.user);
            result.push_str("\nAssistant: ");
            result.push_str(&history.assistant);
            result.push_str("\n\n");
        }
    }
    result.push_str("User: ");
    result.push_str(&turn.input);
    result
}

#[cfg(test)]
mod tests;
