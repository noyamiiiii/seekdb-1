//! MiniVectorDB entry point.
//!
//! The process is a filter over stdin/stdout: it always runs in the foreground,
//! never opens a socket, and writes its data under `/data` (or `--data <dir>`).
//! Protocol mechanics live in the library; this file only parses the command
//! line and wires the statement loop to the engine.

use std::io::{self, BufReader, BufWriter};

use std::process::ExitCode;

use minivectordb::executor::Session;
use minivectordb::response::Response;
use minivectordb::runner::{self, Handler};

const DEFAULT_DATA_DIR: &str = "/data";

fn main() -> ExitCode {
    let mut data_dir = DEFAULT_DATA_DIR.to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = flag_value(&arg, "--data", "-d") {
            data_dir = value.unwrap_or_else(|| args.next().unwrap_or_default());
            if data_dir.is_empty() {
                eprintln!("minivectordb: --data needs a directory");
                return ExitCode::from(2);
            }
            continue;
        }
        if arg == "--help" || arg == "-h" {
            println!("minivectordb [--data DIR] [--mode exact|ann]");
            println!("Reads SQL from stdin, writes results to stdout.");
            return ExitCode::SUCCESS;
        }
        if flag_value(&arg, "--mode", "-m").is_some() {
            // The task lets the driver pick a retrieval mode; this engine always
            // answers exactly, which satisfies both modes' correctness rules.
            if !arg.contains('=') {
                args.next();
            }
            continue;
        }
        if arg.starts_with('-') {
            // Unknown options are ignored instead of being fatal: the test
            // harness owns the command line and may pass extra flags.
            eprintln!("minivectordb: ignoring unknown option {arg}");
            continue;
        }
        // A bare positional argument is the data directory unless it names a
        // retrieval mode.
        if !arg.eq_ignore_ascii_case("exact") && !arg.eq_ignore_ascii_case("ann") {
            data_dir = arg;
        }
    }

    match run(&data_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("minivectordb: {err}");
            ExitCode::from(1)
        }
    }
}

/// Matches `--long value`, `--long=value` and `-s value`. The inner `Option`
/// is the inline value of the `--long=value` form.
fn flag_value(arg: &str, long: &str, short: &str) -> Option<Option<String>> {
    if arg == long || arg == short {
        return Some(None);
    }
    arg.strip_prefix(long)
        .and_then(|rest| rest.strip_prefix('='))
        .map(|value| Some(value.to_string()))
}

fn run(data_dir: &str) -> io::Result<()> {
    eprintln!("minivectordb: data directory {data_dir}");

    // Opening the engine performs recovery (log replay, index rebuild) before
    // the first statement is read; `serve` then writes the READY line.
    let session = Session::open(data_dir).map_err(|err| io::Error::other(err.to_string()))?;
    let mut handler = Engine { session };

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(1 << 16, stdout.lock());
    let mut input = BufReader::with_capacity(1 << 16, stdin.lock());
    runner::serve(&mut handler, &mut input, &mut out)
}

struct Engine {
    session: Session,
}

impl Handler for Engine {
    fn execute(&mut self, statement: &str) -> Response {
        self.session.execute(statement)
    }

    fn shutdown(&mut self) {
        // Every statement already committed or rolled back, but the committed
        // pages still live in the log: checkpoint them so the next start does
        // not replay it.
        if let Err(err) = self.session.shutdown() {
            eprintln!("minivectordb: shutdown flush failed: {err}");
        }
    }
}
