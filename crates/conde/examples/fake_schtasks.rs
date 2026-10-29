//! The Windows tests' stand-in for schtasks.exe, copied into a test machine's PATH: appends its
//! arguments to `schtasks.log` in the directory above its own, answers `/Query` as Task Scheduler
//! does (the task exists once `/Create` ran), and leaves a `started` mark on `/Run`.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::current_exe().expect("own path");
    let root = exe
        .parent()
        .and_then(|d| d.parent())
        .expect("a machine root");
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("schtasks.log"))
        .expect("schtasks.log");
    writeln!(log, "{}", args.join(" ")).expect("logging");
    match args.first().map(String::as_str) {
        Some("/Create") => std::fs::write(root.join("created"), "").expect("mark"),
        Some("/Run") => std::fs::write(root.join("started"), "").expect("mark"),
        Some("/Query") if !root.join("created").exists() => {
            eprintln!("ERROR: The system cannot find the file specified.");
            std::process::exit(1);
        }
        _ => {}
    }
}
