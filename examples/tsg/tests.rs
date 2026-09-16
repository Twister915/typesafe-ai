use clap::Parser;

use super::*;

#[test]
fn cli_rejects_nonfinite_thresholds_and_zero_limits() {
    for args in [
        vec!["tsg", "grep", "condition", "--threshold", "NaN"],
        vec!["tsg", "find", "question", "--threshold", "inf"],
        vec!["tsg", "grep", "condition", "--concurrency", "0"],
        vec!["tsg", "find", "question", "--top", "0"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn command_defaults_preserve_grep_all_match_semantics() {
    let cli = Cli::parse_from(["tsg", "grep", "does this retry?", "src"]);
    let Command::Grep(args) = cli.command else {
        panic!("expected grep command");
    };
    assert_eq!(args.query, "does this retry?");
    assert_eq!(args.paths, [PathBuf::from("src")]);
    assert_eq!(args.concurrency, 64);
    assert_eq!(args.top, None);
    assert_eq!(args.threshold, None);
    assert_eq!(args.preview_lines, 12);
    assert!(!args.full);
}

#[test]
fn display_options_work_before_and_after_the_subcommand() {
    for args in [
        vec!["tsg", "--color", "always", "find", "query", "--full"],
        vec!["tsg", "find", "query", "--color", "always", "--full"],
    ] {
        let cli = Cli::try_parse_from(args).expect("global color and full preview flags");
        assert_eq!(cli.color, terminal::ColorMode::Always);
        let Command::Find(args) = cli.command else {
            panic!("expected find command");
        };
        assert!(args.full);
    }
    assert!(Cli::try_parse_from(["tsg", "grep", "condition", "--preview-lines", "0"]).is_err());
    assert!(
        Cli::try_parse_from(["tsg", "grep", "condition", "--preview-lines", "5", "--full"])
            .is_err()
    );
}

#[derive(Debug)]
pub(crate) struct TestDirectory(pub(crate) PathBuf);

impl TestDirectory {
    pub(crate) fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tsg-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("unique test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
