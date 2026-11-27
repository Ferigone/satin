//! Synchronous classic imports execute on the owning worker thread.
use super::scope;
use crate::{
    loader::DocumentPolicy,
    mime::MimeType,
    network::{CancelSignal, ResponseType},
    script::{
        fatal, webidl,
        workers::{exception, fetch::Network},
    },
};
use rust_browser_javascript::{JsError, Value, Vm};
use url::Url;

pub(super) fn install(
    vm: &mut Vm,
    prototype: Value,
    base: Url,
    network: Option<(Network, CancelSignal)>,
    policy: DocumentPolicy,
) -> Result<(), JsError> {
    let class = vm.get_global("DOMException")?;
    let class = vm.root(class);
    webidl::operation(
        vm,
        prototype,
        "importScripts",
        0,
        move |vm, receiver, args| {
            scope(vm, &receiver)?;
            let addresses = args
                .iter()
                .map(|value| {
                    let text = webidl::usv_string(vm, value, 64 * 1024)?;
                    base.join(&text).map_err(|_| {
                        exception(vm, class, "SyntaxError", "Invalid imported script URL")
                    })
                })
                .collect::<Result<Vec<_>, JsError>>()?;
            for address in addresses {
                let Some((network, generation)) = &network else {
                    return Err(exception(
                        vm,
                        class,
                        "NetworkError",
                        "Worker has no script transport",
                    ));
                };
                let response = network
                    .import_script(address, generation, &policy)
                    .map_err(|error| exception(vm, class, "NetworkError", &error))?;
                if !(200..300).contains(&response.status) {
                    return Err(exception(
                        vm,
                        class,
                        "NetworkError",
                        "Imported script returned a non-OK HTTP status",
                    ));
                }
                let javascript = response
                    .headers
                    .iter()
                    .find(|(name, _)| name == "content-type")
                    .and_then(|(_, value)| MimeType::parse(value))
                    .is_some_and(|mime| mime.javascript());
                if !javascript {
                    return Err(exception(
                        vm,
                        class,
                        "NetworkError",
                        "Imported script must have a JavaScript MIME type",
                    ));
                }
                let Some(bytes) = response.body else {
                    return Err(exception(
                        vm,
                        class,
                        "NetworkError",
                        "Imported script response is missing its body",
                    ));
                };
                let source = encoding_rs::UTF_8.decode_with_bom_removal(&bytes).0;
                let muted = response.response_type == ResponseType::Opaque;
                let referrer = if muted { "about:blank" } else { &response.url };
                if let Err(error) = vm.eval_with_identifier(&source, referrer) {
                    return Err(if muted && !fatal(&error) {
                        exception(
                            vm,
                            class,
                            "NetworkError",
                            "An imported cross-origin script failed",
                        )
                    } else {
                        error
                    });
                }
            }
            Ok(Value::Undefined)
        },
    )?;
    Ok(())
}
