#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

fn main() {
    if let Err(err) = awake_checker::run() {
        awake_checker::win::report_fatal(&err.to_string());
        std::process::exit(1);
    }
}
