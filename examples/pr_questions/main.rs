//! Evaluate repository questions against pull request metadata and a supplied diff.

use std::collections::{BTreeMap, btree_map::Entry};
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::json;
use thiserror::Error;
use typesafe_ai::{Question, Request, UreqClient};

#[cfg(test)]
mod tests;

#[derive(Debug, Parser)]
#[command(about = "Apply repository questions to a pull request title, description, and diff")]
#[command(after_help = "Set TYPESAFE_API_KEY to evaluate. --dry-run needs no key or network.")]
struct Args {
    /// Pull request title.
    #[arg(long, allow_hyphen_values = true)]
    title: String,

    /// Pull request description (empty when omitted).
    #[arg(long, conflicts_with = "description_file", allow_hyphen_values = true)]
    description: Option<String>,

    /// Read the pull request description from a UTF-8 file.
    #[arg(long, value_name = "PATH")]
    description_file: Option<PathBuf>,

    /// UTF-8 diff file, or - to read standard input.
    #[arg(long, default_value = "-", value_name = "PATH")]
    diff: PathBuf,

    /// Directory of question-map JSON files, relative to the current directory.
    #[arg(long, default_value = ".ts_rules", value_name = "PATH")]
    rules_dir: PathBuf,

    /// TypeSafe model identifier.
    #[arg(long, default_value = "jev-latest")]
    model: String,

    /// Validate inputs and print the request JSON without contacting TypeSafe.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Error)]
enum CliError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid questions in {path}: {source}")]
    Questions {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("duplicate question ID {id:?} in {first} and {second}")]
    Duplicate {
        id: String,
        first: PathBuf,
        second: PathBuf,
    },
    #[error("no questions found in {0}; add a .json file containing a question-ID map")]
    NoQuestions(PathBuf),
    #[error("pipe a git diff to stdin, or supply --diff PATH")]
    MissingDiff,
    #[error("TYPESAFE_API_KEY must contain a nonempty UTF-8 API key")]
    ApiKey,
    #[error(transparent)]
    Validation(#[from] typesafe_ai::Error),
    #[error(transparent)]
    Api(#[from] typesafe_ai::UreqError),
    #[error("cannot encode request JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot write stdout: {0}")]
    Output(#[from] io::Error),
}

// Deserialize the map directly so duplicate IDs inside a file cannot silently
// overwrite one another before we check IDs across files.
#[derive(Debug)]
struct Questions(BTreeMap<String, Question>);

impl<'de> Deserialize<'de> for Questions {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct QuestionsVisitor;

        impl<'de> Visitor<'de> for QuestionsVisitor {
            type Value = Questions;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map of question IDs to typed questions")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut questions = BTreeMap::new();
                while let Some((id, question)) = map.next_entry::<String, Question>()? {
                    match questions.entry(id) {
                        Entry::Vacant(entry) => {
                            entry.insert(question);
                        }
                        Entry::Occupied(entry) => {
                            return Err(de::Error::custom(format!(
                                "duplicate question ID {:?}",
                                entry.key()
                            )));
                        }
                    }
                }
                Ok(Questions(questions))
            }
        }

        deserializer.deserialize_map(QuestionsVisitor)
    }
}

fn read_error(path: &Path, source: io::Error) -> CliError {
    CliError::Read {
        path: path.to_owned(),
        source,
    }
}

fn read_text(path: &Path) -> Result<String, CliError> {
    fs::read_to_string(path).map_err(|source| read_error(path, source))
}

fn load_questions(directory: &Path) -> Result<BTreeMap<String, Question>, CliError> {
    let entries = fs::read_dir(directory).map_err(|source| read_error(directory, source))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| read_error(directory, source))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| read_error(&path, source))?;
        if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "json")
        {
            paths.push(path);
        }
    }
    paths.sort();

    let mut questions = BTreeMap::new();
    let mut origins = BTreeMap::new();
    for path in paths {
        let contents = read_text(&path)?;
        let parsed: Questions =
            serde_json::from_str(&contents).map_err(|source| CliError::Questions {
                path: path.clone(),
                source,
            })?;
        for (id, question) in parsed.0 {
            if let Some(first) = origins.insert(id.clone(), path.clone()) {
                return Err(CliError::Duplicate {
                    id,
                    first,
                    second: path,
                });
            }
            questions.insert(id, question);
        }
    }
    if questions.is_empty() {
        return Err(CliError::NoQuestions(directory.to_owned()));
    }
    Ok(questions)
}

fn build_request(args: &Args, stdin: &mut impl Read) -> Result<Request, CliError> {
    let questions = load_questions(&args.rules_dir)?;
    let description = match &args.description_file {
        Some(path) => read_text(path)?,
        None => args.description.clone().unwrap_or_default(),
    };
    let diff = if args.diff == Path::new("-") {
        let mut diff = String::new();
        stdin
            .read_to_string(&mut diff)
            .map_err(|source| read_error(Path::new("stdin"), source))?;
        diff
    } else {
        read_text(&args.diff)?
    };
    let mut request = Request::new(json!({
        "title": args.title,
        "description": description,
        "diff": diff,
    }))
    .with_model(&args.model);
    request.questions = questions;
    request.validate()?;
    Ok(request)
}

fn run(args: Args) -> Result<(), CliError> {
    if args.diff == Path::new("-") && io::stdin().is_terminal() {
        return Err(CliError::MissingDiff);
    }
    let request = build_request(&args, &mut io::stdin().lock())?;
    let mut stdout = io::stdout().lock();
    if args.dry_run {
        serde_json::to_writer_pretty(&mut stdout, &request)?;
    } else {
        let key = env::var("TYPESAFE_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
            .ok_or(CliError::ApiKey)?;
        let client = UreqClient::new(key)?;
        tracing::info!(
            questions = request.questions.len(),
            "evaluating pull request"
        );
        let response = client.evaluate(&request)?;
        // Retain optional usage, extra response fields, and decimal precision.
        stdout.write_all(&response.raw_body)?;
    }
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_ansi(false)
        .with_target(false)
        .without_time()
        .init();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error);
            ExitCode::FAILURE
        }
    }
}
