use super::*;

pub(super) fn background_execution(
    active: bool,
    captured: Option<remagic_core::BackgroundExecution>,
    declared: remagic_core::BackgroundExecution,
    id: &AppId,
) -> Result<remagic_core::BackgroundExecution, String> {
    if active {
        captured.ok_or_else(|| format!("active application {id} lost its scheduling policy"))
    } else {
        Ok(declared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_generation_keeps_captured_policy_across_manifest_reload() {
        let id = AppId::new("reader").unwrap();
        assert_eq!(
            background_execution(
                true,
                Some(remagic_core::BackgroundExecution::Freeze),
                remagic_core::BackgroundExecution::Continue,
                &id,
            ),
            Ok(remagic_core::BackgroundExecution::Freeze)
        );
        assert!(
            background_execution(true, None, remagic_core::BackgroundExecution::Continue, &id,)
                .is_err()
        );
        assert_eq!(
            background_execution(
                false,
                Some(remagic_core::BackgroundExecution::Freeze),
                remagic_core::BackgroundExecution::Continue,
                &id,
            ),
            Ok(remagic_core::BackgroundExecution::Continue)
        );
    }
}
