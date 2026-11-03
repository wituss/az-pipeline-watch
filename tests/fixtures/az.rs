use std::{env, process::{Command, exit}};

fn main() {
    let status = Command::new(env::var_os("WATCH_TEST_PYTHON").expect("Missing test Python"))
        .arg(env::var_os("WATCH_TEST_AZ_SCRIPT").expect("Missing az fixture script"))
        .args(env::args_os().skip(1))
        .status()
        .expect("Cannot run az fixture");
    exit(status.code().unwrap_or(1));
}
