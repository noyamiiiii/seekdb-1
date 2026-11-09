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
        match arg.as_str() {
            "--data" | "-d" => match args.next() {
                Some(v) => data_dir = v,
                None => {
                    eprintln!("minivectordb: --data needs a directory");
                    return ExitCode::from(2);
                }
            },
            "--help" | "-h" => {
                println!("minivectordb [--data DIR]");
                println!("Reads SQL from stdin, writes results to stdout.");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("minivectordb: unknown argument {other}");
                return ExitCode::from(2);
            }
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
