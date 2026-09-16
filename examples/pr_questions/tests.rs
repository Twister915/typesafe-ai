use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use test_case::test_case;

use super::*;

#[derive(Debug)]
struct RulesDirectory(PathBuf);

impl RulesDirectory {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = env::temp_dir().join(format!(
            "typesafe-pr-questions-{}-{nonce}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, contents: &str) {
        fs::write(self.0.join(name), contents).unwrap();
    }

    fn args(&self) -> Args {
        let mut args = Args::parse_from(["pr_questions", "--title", "A PR", "--dry-run"]);
        args.rules_dir.clone_from(&self.0);
        args
    }
}

impl Drop for RulesDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

const NOUL: &str = r#"{"risk":{"type":"noul","instructions":"Does the diff change behavior?"}}"#;

#[test]
fn merges_all_question_types_preserving_structure_and_metadata() {
    let directory = RulesDirectory::new();
    directory.write("b.json", NOUL);
    directory.write(
        "a.json",
        r#"{
            "scope": {
                "type": "choice",
                "instructions": {"question": "What changed?", "inspect": ["title", "diff"]},
                "criteria": {"api": {"meaning": "Public API"}, "other": null}
            },
            "size": {"type": "score", "instructions": ["Rate the diff"], "criteria": [null, {"meaning": "Large"}]}
        }"#,
    );
    directory.write("ignored.txt", "not JSON");
    fs::create_dir(directory.0.join("nested.json")).unwrap();
    fs::write(directory.0.join("nested.json/ignored.json"), NOUL).unwrap();
    let mut args = directory.args();
    args.title = "Quotes: \" and shell text: $(false)".into();
    args.description = Some("First line\nSecond line 🦀".into());
    args.model = "test-model".into();
    let diff = "diff --git a/a.rs b/a.rs\n-old\n+new\n";
    let request = build_request(&args, &mut diff.as_bytes()).unwrap();
    assert_eq!(request.questions.len(), 3);
    assert_eq!(request.model, "test-model");
    assert_eq!(
        request.state,
        json!({"title": args.title, "description": args.description, "diff": diff})
    );
    let wire = serde_json::to_value(&request).unwrap();
    assert_eq!(
        wire["questions"]["scope"]["criteria"]["api"],
        json!({"meaning": "Public API"})
    );
    assert_eq!(
        wire["questions"]["size"]["criteria"][1],
        json!({"meaning": "Large"})
    );
}

#[test]
fn file_inputs_do_not_consume_stdin() {
    let directory = RulesDirectory::new();
    directory.write("rules.json", NOUL);
    directory.write("description.txt", "Description\nwith newlines\n");
    directory.write("diff.txt", "");
    let mut args = directory.args();
    args.description_file = Some(directory.0.join("description.txt"));
    args.diff = directory.0.join("diff.txt");
    let mut stdin = "unused input".as_bytes();
    let request = build_request(&args, &mut stdin).unwrap();
    assert_eq!(request.state["description"], "Description\nwith newlines\n");
    assert_eq!(request.state["diff"], "");
    assert_eq!(stdin, b"unused input");
}

#[test_case("{" ; "malformed JSON")]
#[test_case("[]" ; "array instead of question map")]
#[test_case(r#"{"q":{"type":"unknown","instructions":"Question"}}"# ; "unknown question type")]
#[test_case(r#"{"q":{"type":"choice","instructions":"Question","criteria":{}}}"# ; "empty choice criteria")]
#[test_case(r#"{"q":{"type":"score","instructions":"Question","criteria":["Only one"]}}"# ; "one score level")]
#[test_case(r#"{"q":{"type":"noul","instructions":42}}"# ; "invalid instructions")]
#[test_case(r#"{"q":{"type":"noul","instructions":"One"},"q":{"type":"noul","instructions":"Two"}}"# ; "duplicate IDs within file")]
fn invalid_rules_fail_before_evaluation(contents: &str) {
    let directory = RulesDirectory::new();
    directory.write("rules.json", contents);
    assert!(build_request(&directory.args(), &mut io::empty()).is_err());
}

#[test]
fn duplicate_ids_across_files_report_both_paths() {
    let directory = RulesDirectory::new();
    directory.write("b.json", NOUL);
    directory.write("a.json", NOUL);
    let error = load_questions(&directory.0).unwrap_err();
    let CliError::Duplicate { id, first, second } = error else {
        panic!("expected duplicate ID error, received {error}");
    };
    assert_eq!(id, "risk");
    assert_eq!(first, directory.0.join("a.json"));
    assert_eq!(second, directory.0.join("b.json"));
}

#[test_case(false ; "empty directory")]
#[test_case(true ; "empty map")]
fn empty_rules_are_errors(empty_map: bool) {
    let directory = RulesDirectory::new();
    if empty_map {
        directory.write("rules.json", "{}");
    }
    assert!(matches!(
        load_questions(&directory.0),
        Err(CliError::NoQuestions(_))
    ));
    assert!(matches!(
        load_questions(&directory.0.join("missing")),
        Err(CliError::Read { .. })
    ));
}

#[test]
fn description_sources_are_mutually_exclusive() {
    assert!(
        Args::try_parse_from([
            "pr_questions",
            "--title",
            "Title",
            "--description",
            "Text",
            "--description-file",
            "description.txt",
        ])
        .is_err()
    );
}

#[test]
fn repository_rules_are_valid() {
    let questions =
        load_questions(&Path::new(env!("CARGO_MANIFEST_DIR")).join(".ts_rules")).unwrap();
    let mut request = Request::new(json!({"title": "Example", "description": "", "diff": ""}));
    request.questions = questions;
    request.validate().unwrap();
}

#[cfg(unix)]
#[test]
fn symlinks_are_not_loaded() {
    let directory = RulesDirectory::new();
    directory.write("rules.json", NOUL);
    std::os::unix::fs::symlink("rules.json", directory.0.join("link.json")).unwrap();
    assert_eq!(load_questions(&directory.0).unwrap().len(), 1);
}
