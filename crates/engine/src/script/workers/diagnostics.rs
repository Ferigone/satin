//! Classify media worker messages for opt-in startup traces.
use rust_browser_javascript::StructuredClonePacket;
use std::{
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SATIN_QA_WORKER_MEDIA").is_some())
}

fn known<'a>(packet: &StructuredClonePacket, path: &[&str], labels: &'a [&str]) -> Option<&'a str> {
    let text = packet.diagnostic_string(path)?;
    labels
        .iter()
        .copied()
        .find(|label| text.iter().copied().eq(label.encode_utf16()))
}

fn classify(packet: &StructuredClonePacket) -> Option<(&'static str, Option<&'static str>)> {
    if packet.diagnostic_string(&["wasmBinaryUrl"]).is_some() {
        return Some(("initialize", None));
    }
    packet.diagnostic_number(&["id"])?;
    if let Some(command) = known(
        packet,
        &["funcName"],
        &[
            "create",
            "load",
            "playIntent",
            "play",
            "pause",
            "delete",
            "onMediaCapabilities",
            "onClientSinkError",
            "onClientSinkRecoverableError",
            "setMuted",
            "setVolume",
        ],
    ) {
        return Some((command, None));
    }
    if let Some(event) = known(
        packet,
        &["type"],
        &["PlayerInitialized", "PlayerError", "PlayerRecoverableError"],
    ) {
        return Some((event, None));
    }
    let code = packet
        .diagnostic_number(&["type"])
        .filter(|code| code.fract() == 0. && (0. ..=5.).contains(code))?;
    match code as u8 {
        0 => Some((
            "state",
            known(
                packet,
                &["arg"],
                &["Idle", "Ready", "Buffering", "Playing", "Ended"],
            ),
        )),
        1 => Some((
            "configure",
            known(
                packet,
                &["arg", "0", "mode"],
                &["mse", "mse-worker", "passthrough", "chromecast", "webview"],
            ),
        )),
        2 => Some(("reset", None)),
        4 => Some(("get-decode-info", None)),
        5 => Some((
            "sink-rpc",
            known(
                packet,
                &["arg", "name"],
                &[
                    "enqueue",
                    "addTrack",
                    "setTimestampOffset",
                    "play",
                    "pause",
                    "seekTo",
                    "remove",
                    "endOfStream",
                    "setPlaybackRate",
                    "onSourceDurationChanged",
                ],
            ),
        )),
        _ => None,
    }
}

fn trace(stage: &str, worker: u64, operation: &str, detail: Option<&str>) {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if COUNT.fetch_add(1, Ordering::Relaxed) < 480 {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let detail = detail.unwrap_or("-");
        eprintln!(
            "SATIN_QA_WORKER_MEDIA {timestamp} {stage} worker={worker} operation={operation} detail={detail}"
        );
    }
}

pub(super) fn packet(stage: &str, worker: u64, packet: &StructuredClonePacket) {
    if enabled()
        && let Some((operation, detail)) = classify(packet)
    {
        trace(stage, worker, operation, detail);
    }
}

pub(super) fn message_error(stage: &str, worker: u64) {
    if enabled() {
        trace(stage, worker, "messageerror", None);
    }
}
