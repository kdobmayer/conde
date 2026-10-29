//! The Windows tests' stand-in for Claude Code or Codex: copied to `claude.exe` or `codex.exe`,
//! it runs its arguments as one child process, the way a harness runs a tool, and exits with the
//! child's status. (On Unix the tests use a shell under that name.)

fn main() {
    let mut args = std::env::args_os().skip(1);
    let program = args
        .next()
        .expect("usage: fake_harness <program> [args...]");
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .expect("starting the program");
    std::process::exit(status.code().unwrap_or(1));
}
