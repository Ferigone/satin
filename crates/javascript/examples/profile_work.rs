//! Fixed-input execution with the Windows web VM's work and buffer limits.
#[cfg(windows)]
fn main() {
    use rust_browser_javascript::{Value, Vm, VmLimits, startup_profile};
    let path = std::env::args_os().nth(1).expect("pass one JavaScript file");
    let source = std::fs::read_to_string(path).expect("read UTF-8 source");
    let mut vm = Vm::with_limits(VmLimits {
        buffer_units_per_instruction: 1024,
        ..VmLimits::default()
    });
    let (result, sample) = startup_profile::measure(|| vm.eval(&source));
    let sample = sample.expect("outer script");
    let result = match result {
        Ok(Value::Bool(true)) => String::from("ok"),
        Ok(_) => String::from("wrongResult"),
        Err(error) => format!("{:?}", error.kind),
    };
    println!("{result}\t{}\t{}\t{}", sample.work.iter().sum::<u64>(), sample.wall_us,
        sample.cpu_us.map_or_else(|| "-".to_owned(), |n| n.to_string()));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This probe uses Windows thread counters");
    std::process::exit(2);
}
