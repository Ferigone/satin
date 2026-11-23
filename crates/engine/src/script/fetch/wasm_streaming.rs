//! WebAssembly's Fetch adapter. The current core validates a bounded byte
//! buffer after the body is consumed.
use super::{ResponseData, ResponseType, arg, reject, typed};
use crate::script::webidl;
use rust_browser_javascript::{JsError, Value, Vm};

#[derive(Clone)]
struct Completion {
    core: Value,
    arguments: [Value; 2],
    arity: usize,
    resolve: Value,
    reject: Value,
}

impl Completion {
    fn keep(&self, vm: &mut Vm, callback: &Value) -> Result<(), JsError> {
        for (index, value) in [
            &self.core,
            &self.arguments[0],
            &self.arguments[1],
            &self.resolve,
            &self.reject,
        ]
        .into_iter()
        .enumerate()
        {
            vm.set_host_slot(callback.clone(), index as u32, value.clone())?;
        }
        Ok(())
    }

    fn read_response(&self, vm: &mut Vm, response: Value, reader: Value) -> Result<(), JsError> {
        check_response(vm, &response)?;
        // Capture the native reader at installation. It enforces the Body
        // mixin's locked/disturbed checks and the existing read budget.
        let bytes = vm.call(reader, response, &[])?;
        let completion = self.clone();
        let compile =
            vm.new_native_function("Complete WebAssembly body read", move |vm, _, args| {
                let arguments = [
                    arg(args, 0),
                    completion.arguments[0].clone(),
                    completion.arguments[1].clone(),
                ];
                let result = vm.call(
                    completion.core.clone(),
                    Value::Undefined,
                    &arguments[..completion.arity],
                );
                match result {
                    Ok(promise) => {
                        vm.then_intrinsic(
                            promise,
                            completion.resolve.clone(),
                            completion.reject.clone(),
                        )?;
                    }
                    Err(error) => reject(vm, completion.reject.clone(), error)?,
                }
                Ok(Value::Undefined)
            });
        self.keep(vm, &compile)?;
        vm.then_intrinsic(bytes, compile, self.reject.clone())?;
        Ok(())
    }
}

fn check_response(vm: &Vm, response: &Value) -> Result<(), JsError> {
    let data = typed::<ResponseData>(vm, response, "Response")?;
    let data = data.borrow();
    let headers = data.headers.borrow();
    let mut content_types = headers
        .list
        .iter()
        .filter(|(name, _)| name == "content-type");
    let correct_type = content_types.next().is_some_and(|(_, value)| {
        value
            .trim_matches([' ', '\t'])
            .eq_ignore_ascii_case("application/wasm")
    });
    if !correct_type || content_types.next().is_some() {
        return Err(JsError::type_error(
            "WebAssembly response requires application/wasm",
        ));
    }
    if !matches!(
        data.response.response_type,
        ResponseType::Basic | ResponseType::Cors | ResponseType::Default
    ) || !(200..=299).contains(&data.response.status)
    {
        return Err(JsError::type_error(
            "WebAssembly response is unavailable or unsuccessful",
        ));
    }
    Ok(())
}

fn start(
    vm: &mut Vm,
    args: &[Value],
    core: Value,
    reader: Value,
    instantiate: bool,
) -> Result<Value, JsError> {
    let result = vm.new_promise_capability()?;
    let arguments = [arg(args, 1), arg(args, 2)];
    if instantiate && !arguments[0].is_undefined() && arguments[0].as_object().is_none() {
        reject(
            vm,
            result.reject,
            JsError::type_error("WebAssembly import object must be an object"),
        )?;
        return Ok(result.promise);
    }
    // Web IDL adopts the caller's value through an intrinsic resolve function.
    // This honors thenables at the API boundary. Later reactions use VM records.
    let source = vm.new_promise_capability()?;
    vm.call(source.resolve, Value::Undefined, &[arg(args, 0)])?;
    let completion = Completion {
        core,
        arguments,
        arity: if instantiate { 3 } else { 2 },
        resolve: result.resolve,
        reject: result.reject,
    };
    let captured = completion.clone();
    let native_reader = reader.clone();
    let receive = vm.new_native_function("Receive WebAssembly response", move |vm, _, args| {
        if let Err(error) = captured.read_response(vm, arg(args, 0), native_reader.clone()) {
            reject(vm, captured.reject.clone(), error)?;
        }
        Ok(Value::Undefined)
    });
    completion.keep(vm, &receive)?;
    vm.set_host_slot(receive.clone(), 5, reader)?;
    vm.then_intrinsic(source.promise, receive, completion.reject)?;
    Ok(result.promise)
}

pub(super) fn install(vm: &mut Vm, response: &Value) -> Result<(), JsError> {
    let namespace = vm.get_global("WebAssembly")?;
    let reader = vm.get_property(response.clone(), "arrayBuffer")?;
    for (name, intrinsic, instantiate) in [
        ("compileStreaming", "compile", false),
        ("instantiateStreaming", "instantiate", true),
    ] {
        let core = vm.get_property(namespace.clone(), intrinsic)?;
        let captured_core = core.clone();
        let captured_reader = reader.clone();
        let method = webidl::operation(vm, namespace.clone(), name, 1, move |vm, _, args| {
            start(
                vm,
                args,
                captured_core.clone(),
                captured_reader.clone(),
                instantiate,
            )
        })?;
        vm.set_host_slot(method.clone(), 0, core)?;
        vm.set_host_slot(method, 1, reader.clone())?;
    }
    Ok(())
}
