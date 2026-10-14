//! The runner of a `harness = false` test binary that changes the process environment: its
//! cases run one after another on the main thread, so no second test thread exists to read the
//! environment while a case changes it. A case is listed in the binary's `main`, not marked
//! `#[test]` (without the libtest harness such a function would not be compiled in).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

pub type Case = (&'static str, fn());

/// Runs `cases` (filtered by the first non-flag argument, as libtest does), prints one
/// `test <name> ... ok` line each and a summary, and exits non-zero if any failed.
pub fn run(binary: &str, cases: &[Case]) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        for (name, _) in cases {
            println!("{name}: test");
        }
        return;
    }
    let filter = args.iter().find(|a| !a.starts_with('-'));
    // none of these cases is ignored
    let ignored_only = args.iter().any(|a| a == "--ignored");
    let (mut passed, mut failed) = (0, Vec::new());
    for (name, case) in cases {
        if ignored_only || filter.is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let start = Instant::now();
        match catch_unwind(AssertUnwindSafe(case)) {
            Ok(()) => {
                println!("test {name} ... ok ({} ms)", start.elapsed().as_millis());
                passed += 1;
            }
            Err(_) => {
                println!("test {name} ... FAILED");
                failed.push(*name);
            }
        }
    }
    if failed.is_empty() {
        println!("\n{binary}: ok. {passed} passed; 0 failed\n");
    } else {
        println!("\n{binary}: FAILED. {passed} passed; {} failed: {}\n", failed.len(), failed.join(", "));
        std::process::exit(101);
    }
}
