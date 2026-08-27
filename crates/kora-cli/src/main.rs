//! The `kora` command-line tool.
//!
//! Phase 0: only `--version`. Subcommands (`run`, `test`, `audit`, `trace`)
//! arrive with their phases — see DECISIONS.md.

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("-V") | Some("version") => {
            println!("kora {VERSION}");
        }
        Some(other) => {
            eprintln!("kora: unknown command `{other}`");
            eprintln!("usage: kora --version");
            std::process::exit(2);
        }
        None => {
            println!("Kora — an agent-first programming language");
            println!("usage: kora --version");
        }
    }
}
