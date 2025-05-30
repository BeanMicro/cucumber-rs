use std::{borrow::Cow, fmt::Debug, mem, sync::LazyLock};

use cucumber::{Event, Writer, cli, event, given, parser, then, when};
use regex::Regex;

#[derive(Debug, Default, cucumber::World)]
struct World(usize);

#[given(regex = r"foo is (\d+)")]
#[when(regex = r"foo is (\d+)")]
#[then(regex = r"foo is (\d+)")]
fn step(w: &mut World, num: usize) {
    assert_eq!(w.0, num);
    w.0 += 1;
}

#[given(regex = r"foo is (\d+) ambiguous")]
fn ambiguous(_w: &mut World) {}

#[derive(Default)]
struct DebugWriter {
    output: String,
    first_line_printed: bool,
}

impl<World: 'static + Debug> Writer<World> for DebugWriter {
    type Cli = cli::Empty;

    async fn handle_event(
        &mut self,
        ev: parser::Result<Event<event::Cucumber<World>>>,
        _: &Self::Cli,
    ) {
        let ev: Cow<_> = match ev.map(Event::into_inner) {
            Err(_) => "ParsingError".into(),
            Ok(ev) => format!("{ev:?}").into(),
        };

        let without_span = SPAN_OR_PATH_RE.replace_all(ev.as_ref(), "");

        if mem::replace(&mut self.first_line_printed, true) {
            self.output.push('\n');
        }
        self.output.push_str(without_span.as_ref());
    }
}

/// [`Regex`] to unify spans and file paths on Windows, Linux and macOS for
/// tests.
// TODO: Switch back to `lazy-regex::regex!()` once it migrates to `std`:
//       https://github.com/Canop/lazy-regex/issues/10
static SPAN_OR_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        "( span: Span \\{ start: (\\d+), end: (\\d+) },\
         |, col: (\\d+)\
         | path: (None|(Some\\()?\"[^\"]*\")\\)?,?)",
    )
    .unwrap()
});

#[cfg(test)]
mod spec {
    use std::{fmt, fs, io, sync::LazyLock};

    use cucumber::{
        World as _, WriterExt as _,
        writer::{self, Coloring},
    };
    use globwalk::GlobWalkerBuilder;
    use itertools::Itertools;
    use regex::{Captures, Match, Regex};

    use super::{DebugWriter, World};

    /// [`Regex`] to transform full paths (both unix-like and windows) to a
    /// relative paths.
    // TODO: Switch back to `lazy-regex::regex!()` once it migrates to `std`:
    //       https://github.com/Canop/lazy-regex/issues/10
    static FULL_PATH: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"((\?|\\|\/).*(\\|\/))?tests((\\|\/)\w*)*").unwrap()
    });

    /// Replaces [`FULL_PATH`] with a relative path.
    fn relative_path(cap: &Captures<'_>) -> String {
        format!(
            "tests{}",
            cap[0].split("tests").skip(1).join("tests").replace('\\', "/")
        )
    }

    /// [`Regex`] to make `cargo careful` assertion output to match `cargo test`
    /// output.
    // TODO: Switch back to `lazy-regex::regex!()` once it migrates to `std`:
    //       https://github.com/Canop/lazy-regex/issues/10
    static CAREFUL_ASSERTION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            "assertion `left == right` failed(:)?\
             (.*)\
             \n(\\s+)left: (.+)\
             \n(\\s+)right: (\\w+)",
        )
        .unwrap()
    });

    /// Replaces [`CAREFUL_ASSERTION`] with `cargo test` output.
    fn unify_asserts(cap: &Captures<'_>) -> String {
        format!(
            "assertion failed: `(left == right)`{}\
             {}\
             \n{}left: `{}`,\
             \n{}right: `{}`",
            cap.get(1).as_ref().map_or("", Match::as_str),
            &cap[2],
            &cap[3],
            &cap[4],
            &cap[5],
            &cap[6],
        )
    }

    /// Deterministic output of a [`writer::Basic`].
    #[derive(Clone, Debug, Default)]
    struct Output(Vec<u8>);

    impl io::Write for Output {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl fmt::Display for Output {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let o = String::from_utf8(self.0.clone())
                .unwrap_or_else(|e| panic!("`Output` is not a string: {e}"));
            let o = CAREFUL_ASSERTION.replace_all(&o, unify_asserts);
            let o = FULL_PATH.replace_all(&o, relative_path);
            write!(f, "{o}")
        }
    }

    /// Loads a file from the file system as a string.
    fn load_file(path: impl AsRef<str>) -> String {
        let path = path.as_ref();
        fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("Failed to load `{path}` file: {e}"))
            .replace("\r\n", "\n")
    }

    #[tokio::test]
    async fn test() {
        let walker =
            GlobWalkerBuilder::new("tests/features/output", "*.feature")
                .case_insensitive(true)
                .build()
                .unwrap();
        let files = walker
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_str().unwrap().to_owned())
            .collect::<Vec<String>>();

        assert_eq!(
            files.len(),
            fs::read_dir("tests/features/output").unwrap().count() / 4,
            "Not all `.feature` files were collected",
        );

        for file in files {
            let expected =
                load_file(format!("tests/features/output/{file}.debug.out"));
            let debug = World::cucumber()
                .with_writer(DebugWriter::default().normalized())
                .with_default_cli()
                .run(format!("tests/features/output/{file}"))
                .await;
            assert_eq!(
                debug.output.trim(),
                expected.trim(),
                "\n[debug] file: {file}",
            );

            let expected =
                load_file(format!("tests/features/output/{file}.basic.out"));
            let mut output = Output::default();
            _ = World::cucumber()
                .with_writer(
                    writer::Basic::raw(&mut output, Coloring::Never, 0)
                        .discard_stats_writes()
                        .normalized(),
                )
                .with_default_cli()
                .run(format!("tests/features/output/{file}"))
                .await;
            assert_eq!(
                output.to_string().trim(),
                expected.trim(),
                "\n[basic] file: {file}",
            );

            let expected =
                load_file(format!("tests/features/output/{file}.colored.out"));
            let mut output = Output::default();
            _ = World::cucumber()
                .with_writer(
                    writer::Basic::raw(&mut output, Coloring::Always, 0)
                        .discard_stats_writes()
                        .normalized(),
                )
                .with_default_cli()
                .run(format!("tests/features/output/{file}"))
                .await;
            assert_eq!(
                output.to_string().trim(),
                expected.trim(),
                "\n[colored] file: {file}",
            );
        }
    }
}
