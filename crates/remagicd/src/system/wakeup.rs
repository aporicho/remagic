use std::fs;
use std::path::Path;

const WAKELOCK_NAME: &str = "remagic-managed";
const WAKEUP_SOURCE_SUMMARY_LIMIT: usize = 6;
const WAKEUP_SOURCE_ROOT: &str = "/sys/class/wakeup";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct WakeupSnapshot {
    pub(super) available: bool,
    pub(super) sources: Vec<WakeupSource>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct WakeupSource {
    pub(super) name: String,
    pub(super) active_count: u64,
    pub(super) event_count: u64,
    pub(super) wakeup_count: u64,
    pub(super) active_time_ms: u64,
    pub(super) prevent_suspend_time_ms: u64,
}

pub(super) fn read_wakeup_snapshot() -> WakeupSnapshot {
    let entries = match fs::read_dir(WAKEUP_SOURCE_ROOT) {
        Ok(entries) => entries,
        Err(_) => return WakeupSnapshot::default(),
    };
    let mut sources = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(source) = read_wakeup_source(&path) {
            sources.push(source);
        }
    }
    WakeupSnapshot {
        available: true,
        sources,
    }
}

fn read_wakeup_source(path: &Path) -> Option<WakeupSource> {
    let name = read_trimmed_path(&path.join("name")).or_else(|| {
        path.file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
    })?;
    Some(WakeupSource {
        name,
        active_count: read_u64_path(&path.join("active_count")),
        event_count: read_u64_path(&path.join("event_count")),
        wakeup_count: read_u64_path(&path.join("wakeup_count")),
        active_time_ms: read_u64_path(&path.join("active_time_ms")),
        prevent_suspend_time_ms: read_u64_path(&path.join("prevent_suspend_time_ms")),
    })
}

pub(super) fn active_wakeup_source_summary(snapshot: &WakeupSnapshot) -> String {
    if !snapshot.available {
        return "unavailable".into();
    }
    let mut active: Vec<_> = snapshot
        .sources
        .iter()
        .filter(|source| source.name != WAKELOCK_NAME && source.active_time_ms > 0)
        .collect();
    active.sort_by(|left, right| {
        right
            .active_time_ms
            .cmp(&left.active_time_ms)
            .then_with(|| left.name.cmp(&right.name))
    });
    if active.is_empty() {
        return "none".into();
    }
    active
        .into_iter()
        .take(WAKEUP_SOURCE_SUMMARY_LIMIT)
        .map(|source| format!("{}(active_ms={})", source.name, source.active_time_ms))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn wakeup_source_delta_summary(
    before: &WakeupSnapshot,
    after: &WakeupSnapshot,
) -> String {
    if !after.available {
        return "unavailable".into();
    }
    let mut deltas = Vec::new();
    for source in &after.sources {
        if source.name == WAKELOCK_NAME {
            continue;
        }
        let baseline = before
            .sources
            .iter()
            .find(|candidate| candidate.name == source.name);
        let active_delta = baseline.map_or(source.active_count, |value| {
            source.active_count.saturating_sub(value.active_count)
        });
        let event_delta = baseline.map_or(source.event_count, |value| {
            source.event_count.saturating_sub(value.event_count)
        });
        let wakeup_delta = baseline.map_or(source.wakeup_count, |value| {
            source.wakeup_count.saturating_sub(value.wakeup_count)
        });
        let prevent_delta = baseline.map_or(source.prevent_suspend_time_ms, |value| {
            source
                .prevent_suspend_time_ms
                .saturating_sub(value.prevent_suspend_time_ms)
        });
        if source.active_time_ms == 0
            && active_delta == 0
            && event_delta == 0
            && wakeup_delta == 0
            && prevent_delta == 0
        {
            continue;
        }
        deltas.push(WakeupSourceDelta {
            name: source.name.clone(),
            active_time_ms: source.active_time_ms,
            active_delta,
            event_delta,
            wakeup_delta,
            prevent_delta,
        });
    }
    deltas.sort_by(|left, right| {
        right
            .active_time_ms
            .cmp(&left.active_time_ms)
            .then_with(|| right.prevent_delta.cmp(&left.prevent_delta))
            .then_with(|| right.event_delta.cmp(&left.event_delta))
            .then_with(|| left.name.cmp(&right.name))
    });
    if deltas.is_empty() {
        return "none".into();
    }
    deltas
        .into_iter()
        .take(WAKEUP_SOURCE_SUMMARY_LIMIT)
        .map(|delta| {
            format!(
                "{}(active_ms={}, active+{}, events+{}, wakeups+{}, prevent_ms+{})",
                delta.name,
                delta.active_time_ms,
                delta.active_delta,
                delta.event_delta,
                delta.wakeup_delta,
                delta.prevent_delta
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WakeupSourceDelta {
    name: String,
    active_time_ms: u64,
    active_delta: u64,
    event_delta: u64,
    wakeup_delta: u64,
    prevent_delta: u64,
}

fn read_u64_path(path: &Path) -> u64 {
    read_trimmed_path(path)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn read_trimmed_path(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
