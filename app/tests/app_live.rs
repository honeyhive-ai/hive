//! In-app live test: the real turn path (`send_message` → `run_turn` →
//! `run_prepared_turn`) over a real `AppState`, with no window. See
//! `app/src/e2e.rs` for the scenarios and environment. Skipped unless built
//! with `--features e2e` and `HIVE_APP_LIVE_ENDPOINT` is set:
//!
//! ```text
//! HIVE_APP_LIVE_ENDPOINT=http://host:8080 \
//!   cargo test -p Hive --features e2e --test app_live -- --nocapture
//! ```
//!
//! `harness = false`: macOS only lets the main thread create the app's
//! event loop, and libtest runs tests on worker threads.

fn main() {
    #[cfg(feature = "e2e")]
    {
        let failed = hive_app_lib::e2e::run_live();
        if failed > 0 {
            eprintln!("{failed} scenario(s) failed");
            std::process::exit(1);
        }
    }
    #[cfg(not(feature = "e2e"))]
    eprintln!("built without --features e2e; skipping the in-app live test");
}
