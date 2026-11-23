//! Resolve Blob URLs inside the requesting document. The network loader never
//! receives an object URL or has access to a document's registry.
use super::{FetchHost, ResponseBody, Shared};
use crate::{
    network::{FetchError, FetchErrorKind, ResponseType, WebRequest, WebResponse},
    script::{blobs::Blob, schedule_timer},
};
use rust_browser_javascript::{JsError, Value, Vm};
use std::{ops::Range, rc::Rc, time::Duration};

pub(super) fn queue(vm: &mut Vm, state: &Shared, id: u64) -> Result<(), JsError> {
    let document = state
        .borrow()
        .document
        .upgrade()
        .ok_or_else(|| JsError::type_error("Blob Fetch requires a document"))?;
    let owner = Rc::downgrade(state);
    let task = vm.new_native_function("Complete Blob Fetch", move |vm, _, _| {
        let Some(state) = owner.upgrade() else {
            return Ok(Value::Undefined);
        };
        let result = state
            .borrow_mut()
            .pending
            .get_mut(&id)
            .and_then(|pending| pending.blob.take());
        if let Some(result) = result {
            FetchHost { state }.complete_response(vm, id, result)?;
        }
        Ok(Value::Undefined)
    });
    schedule_timer(vm, &document, task, Vec::new(), Duration::ZERO)?;
    Ok(())
}

pub(super) fn response(
    request: &WebRequest,
    blob: Option<Rc<Blob>>,
) -> Result<(WebResponse, ResponseBody), FetchError> {
    if request.method != "GET" {
        return Err(FetchError::invalid("Blob URLs require GET"));
    }
    let blob = blob.ok_or_else(|| {
        FetchError::new(
            FetchErrorKind::Network,
            "Blob URL is unavailable in this document",
        )
    })?;
    let length = blob.bytes().len();
    let requested_range = request
        .headers
        .iter()
        .find_map(|(name, value)| (name == "range").then_some(value));
    let (status, status_text, range) = if let Some(value) = requested_range {
        let range = parse_range(value, length)
            .ok_or_else(|| FetchError::invalid("Invalid Blob URL byte range"))?;
        (206, "Partial Content", range)
    } else {
        (200, "OK", 0..length)
    };
    let mut headers = vec![
        ("content-length".into(), range.len().to_string()),
        ("content-type".into(), blob.mime.to_string()),
    ];
    if requested_range.is_some() {
        headers.push((
            "content-range".into(),
            format!("bytes {}-{}/{length}", range.start, range.end - 1),
        ));
    }
    let mut url = request.url.clone();
    url.set_fragment(None);
    Ok((
        WebResponse {
            url: url.into(),
            status,
            status_text: status_text.into(),
            headers,
            body: None,
            response_type: ResponseType::Basic,
            redirected: false,
        },
        ResponseBody::Blob(blob, range),
    ))
}

fn parse_range(value: &str, length: usize) -> Option<Range<usize>> {
    let (unit, bounds) = value.split_once('=')?;
    let (first, last) = bounds.split_once('-')?;
    if unit.trim_end_matches([' ', '\t']) != "bytes" || length == 0 {
        return None;
    }
    let first = decimal(first.trim_matches([' ', '\t']))?;
    let last = decimal(last.trim_matches([' ', '\t']))?;
    match (first, last) {
        (Some(start), end) if start < length => {
            let end = end.unwrap_or(length - 1).min(length - 1);
            (start <= end).then_some(start..end + 1)
        }
        (None, Some(suffix)) if suffix != 0 => Some(length.saturating_sub(suffix)..length),
        _ => None,
    }
}

fn decimal(value: &str) -> Option<Option<usize>> {
    if value.is_empty() {
        return Some(None);
    }
    value
        .bytes()
        .try_fold(0usize, |number, digit| {
            digit.is_ascii_digit().then(|| {
                number
                    .saturating_mul(10)
                    .saturating_add(usize::from(digit - b'0'))
            })
        })
        .map(Some)
}

#[cfg(test)]
mod tests {
    use crate::{LoadedPage, script::ScriptRuntime};
    use std::sync::Arc;
    use url::Url;

    #[test]
    fn dropping_document_releases_registered_and_pending_blob_backings() {
        let mut runtime =
            ScriptRuntime::new(&LoadedPage::from_html("data:text/html,owner", "")).unwrap();
        let track_blob = |runtime: &mut ScriptRuntime, text: &str| {
            let address = runtime
                .eval(&format!("URL.createObjectURL(new Blob(['{text}']))"))
                .unwrap()
                .as_string()
                .unwrap()
                .to_string_lossy();
            let blob = runtime
                .state
                .borrow()
                .media
                .resolve_blob(&runtime.vm, &Url::parse(&address).unwrap())
                .unwrap();
            let (bytes, _) = blob.shared_bytes();
            (address, Arc::downgrade(&bytes))
        };
        let (_, registered) = track_blob(&mut runtime, "registered");
        let (address, pending) = track_blob(&mut runtime, "pending");
        runtime
            .eval(&format!(
                "fetch('{address}'); URL.revokeObjectURL('{address}');"
            ))
            .unwrap();
        assert!(
            runtime
                .state
                .borrow()
                .media
                .resolve_blob(&runtime.vm, &Url::parse(&address).unwrap())
                .is_none()
        );
        // Collect the revoked wrapper without running the queued Fetch task.
        runtime.vm.collect_garbage();
        assert_eq!(runtime.fetch.pending_count(), 1);
        assert!(registered.upgrade().is_some());
        assert!(pending.upgrade().is_some());

        drop(runtime);
        assert!(registered.upgrade().is_none());
        assert!(pending.upgrade().is_none());
    }
}
