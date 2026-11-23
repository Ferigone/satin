//! Opt-in Windows process-memory measurements, separate from regressions.
#![cfg(windows)]

use rust_browser_javascript::Vm;
use std::process::Command;

fn sample(vm: &Vm, stage: &str) {
    let command = format!(
        "$p = Get-Process -Id {}; '{{0}} {{1}}' -f $p.PrivateMemorySize64, $p.WorkingSet64",
        std::process::id()
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    let (private, working_set) = output.trim().split_once(' ').unwrap();
    println!(
        "{stage}: private_bytes={private} working_set_bytes={working_set} vm_estimated_bytes={}",
        vm.estimated_heap_bytes()
    );
}

#[test]
#[ignore = "manual Windows process-memory probe"]
fn windows_linear_memory_512_page_os_probe() {
    let mut vm = Vm::new();
    vm.collect_garbage();
    sample(&vm, "before");
    vm.eval("var mem = new WebAssembly.Memory({initial:512, maximum:1024});")
        .unwrap();
    sample(&vm, "memory_only");
    vm.eval(
        r#"
        var u8 = new Uint8Array(mem.buffer), u32 = new Uint32Array(mem.buffer),
            data = new DataView(mem.buffer);
        for (var i = 0; i < u8.length; i += 4096) u8[i] = 1;
        "#,
    )
    .unwrap();
    sample(&vm, "three_views_pages_touched");
    vm.eval("mem = u8 = u32 = data = null;").unwrap();
    vm.collect_garbage();
    sample(&vm, "after_gc");
}
