//! Opt-in instrumentation used only by the public Twitch replay.
use super::{JsError, ScriptRuntime, Value, Vm};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct Profile {
    core_instructions: u64,
    always_collect: bool,
}

impl ScriptRuntime {
    /// Installs the replay's module hook in this document and enables cost measurements.
    pub fn enable_twitch_bootstrap_profile(
        &mut self,
        core_instructions: u64,
        always_collect: bool,
    ) -> Result<(), JsError> {
        let callback = self
            .vm
            .new_native_function("__satinProfile", |vm, _, args| {
                if let [Value::Number(enter), Value::Number(id), ..] = args {
                    vm.qa_factory(*enter != 0.0, *id as u64);
                    println!(
                        "FACTORY {} {} {} {} {}",
                        *enter as u64,
                        *id as u64,
                        vm.qa_fuel(),
                        vm.estimated_heap_bytes(),
                        vm.heap_cell_count()
                    );
                }
                Ok(Value::Undefined)
            });
        self.vm.set_global("__satinProfile", callback)?;
        self.twitch_profile = Some(Profile {
            core_instructions,
            always_collect,
        });
        Ok(())
    }
}

impl Profile {
    pub(super) fn run(
        self,
        vm: &mut Vm,
        identifier: &str,
        run: impl FnOnce(&mut Vm) -> Result<(), JsError>,
    ) -> Result<(), JsError> {
        let core = identifier.starts_with("https://assets.twitch.tv/assets/core-");
        let budget = if core {
            self.core_instructions
        } else {
            vm.limits().max_instructions
        };
        if core {
            vm.qa_begin();
        }
        let start = Instant::now();
        let result = vm.with_task_limits(budget, Some(start + Duration::from_secs(120)), run);
        if core {
            vm.qa_finish();
            let error = result.as_ref().err().map(|e| (&e.kind, &e.message));
            println!(
                "CORE elapsed={:?} budget={budget} remaining={} bytes={} cells={} error={error:?}",
                start.elapsed(),
                vm.qa_fuel(),
                vm.estimated_heap_bytes(),
                vm.heap_cell_count()
            );
        }
        result
    }

    pub(super) fn collect(self, runtime: &mut ScriptRuntime) {
        let start = Instant::now();
        if self.always_collect {
            runtime.vm.collect_garbage();
        } else {
            runtime.collect_after_task(true);
        }
        eprintln!("GC {:.6}", start.elapsed().as_secs_f64());
    }
}
