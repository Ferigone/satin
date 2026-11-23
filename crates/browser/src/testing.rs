//! Deterministic input through our application's handlers, never OS input.
use serde::Deserialize;
use std::{collections::VecDeque, path::Path};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SmokeAction {
    Click {
        id: String,
    },
    Text {
        value: String,
    },
    Key {
        value: String,
        #[serde(default)]
        control: bool,
        #[serde(default)]
        shift: bool,
        #[serde(default)]
        alt: bool,
    },
    Title {
        value: String,
    },
    WaitTitle {
        value: String,
        timeout_ms: u64,
    },
}
pub fn read_actions(path: &Path) -> Result<VecDeque<SmokeAction>, String> {
    if std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len()
        > 1024 * 1024
    {
        return Err("Smoke actions exceed 1 MiB".into());
    }
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let actions: VecDeque<SmokeAction> =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if actions.len() > 256 {
        return Err("Smoke actions exceed 256 steps".into());
    }
    Ok(actions)
}
