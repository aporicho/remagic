use super::*;

impl Daemon {
    /// Queue an application-initiated transition without waiting for the
    /// transition itself. Acceptance means the serialized manager event loop
    /// owns the request; launch readiness is deliberately not part of this
    /// acknowledgement, otherwise the calling foreground app can deadlock its
    /// own park handshake.
    pub(in crate::daemon) async fn enqueue_detached(&self, event: Event) -> Result<(), String> {
        enqueue_detached_event(&self.events, &self.launch_interrupt_epoch, event).await
    }
}

async fn enqueue_detached_event(
    events: &tokio::sync::mpsc::Sender<QueuedEvent>,
    launch_interrupt_epoch: &AtomicU64,
    event: Event,
) -> Result<(), String> {
    tokio::time::timeout(
        Duration::from_secs(2),
        events.send(QueuedEvent::unattended(event, launch_interrupt_epoch)),
    )
    .await
    .map_err(|_| "manager request timed out before it could be queued".to_string())?
    .map_err(|_| "manager event loop is unavailable".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_app_ack_does_not_wait_for_a_blocked_launch_handler() {
        let (events, mut receiver) = tokio::sync::mpsc::channel(1);
        let epoch = AtomicU64::new(5);
        let app = AppId::new("koreader").unwrap();

        tokio::time::timeout(
            Duration::from_millis(100),
            enqueue_detached_event(&events, &epoch, Event::Launch(app.clone(), None)),
        )
        .await
        .expect("queue acknowledgement must not wait for launch completion")
        .unwrap();

        let queued = receiver.recv().await.expect("launch must be queued");
        assert!(matches!(queued.event, Event::Launch(id, None) if id == app));
        assert!(queued.reply.is_none());
        assert!(!queued.request_fence.is_cancelled());
    }
}
