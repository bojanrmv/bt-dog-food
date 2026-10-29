//! The labelled C# corpus (tests/corpus/csharp): dgf-dup's scores on it, held
//! at or above the levels reached so far, and the labels themselves, checked
//! by running the corpus code (needs a .NET 10 SDK; skipped without one, or
//! with DGF_SKIP_E2E=1).

use std::path::Path;
use std::process::Command;

use dog_food_dup::bench::{render, score, Labels};
use dog_food_dup::{report, scan, Settings};

const CORPUS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus/csharp");

#[test]
fn dgf_dup_keeps_its_scores_on_the_corpus() {
    let corpus = Path::new(CORPUS);
    let labels =
        Labels::parse(&std::fs::read_to_string(corpus.join("cases.json")).unwrap()).unwrap();
    let found = scan(&[corpus.join("Cases")], corpus, true, Settings::default());
    let json = serde_json::from_str(&report::json(&found)).unwrap();
    let card = score(&json, &labels);
    println!("{}", render(&card, None));
    assert!(card.equal_wrong.is_empty(), "{:?}", card.equal_wrong);
    assert!(card.unrelated.is_empty(), "{:?}", card.unrelated);
    // The levels reached so far; raise them as the finder improves.
    assert!(card.recall_of(1) >= 1.0);
    assert!(card.recall_of(2) >= 1.0);
    assert!(card.recall_of(3) >= 0.9);
}

#[test]
fn the_corpus_labels_hold_when_the_code_runs() {
    if !has_dotnet10() {
        eprintln!("skipped: needs a .NET 10 SDK");
        return;
    }
    let artifacts = Path::new(env!("CARGO_TARGET_TMPDIR")).join("dup-corpus");
    let output = Command::new("dotnet")
        .args(["run", "-c", "Release", "--project"])
        .arg(Path::new(CORPUS).join("Corpus.csproj"))
        .arg("--artifacts-path")
        .arg(&artifacts)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("0 labels not borne out"), "{text}");
}

#[test]
fn with_the_csharp_engine_every_rewrite_is_found_and_no_near_miss_is_called_equal() {
    if !has_dotnet10() {
        eprintln!("skipped: needs a .NET 10 SDK");
        return;
    }
    let corpus = Path::new(CORPUS);
    let artifacts = Path::new(env!("CARGO_TARGET_TMPDIR")).join("dgf-dup-cs");
    let engine = Path::new(env!("CARGO_MANIFEST_DIR")).join("semantic/DgfDupCs.csproj");
    let built = Command::new("dotnet")
        .args(["build", "-c", "Release", "-o"])
        .arg(&artifacts)
        .arg(&engine)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stdout)
    );
    let paths = [corpus.join("Cases"), corpus.join("Model.cs")];
    let mut found = scan(&paths, corpus, true, Settings::default());
    let command = dog_food_dup::semantic::command(Some(&artifacts.join("dgf-dup-cs.dll")));
    let classes = dog_food_dup::semantic::classes(command, &paths, corpus, true).unwrap();
    dog_food_dup::semantic::merge(&mut found, classes, corpus);
    let labels =
        Labels::parse(&std::fs::read_to_string(corpus.join("cases.json")).unwrap()).unwrap();
    let card = score(
        &serde_json::from_str(&report::json(&found)).unwrap(),
        &labels,
    );
    println!("{}", render(&card, None));
    assert!(card.equal_wrong.is_empty(), "{:?}", card.equal_wrong);
    assert!(card.unrelated.is_empty(), "{:?}", card.unrelated);
    assert!(card.recall_of(4) >= 1.0);
    assert!(card.recall() >= 0.95);
}

fn has_dotnet10() -> bool {
    let sdks = Command::new("dotnet").arg("--list-sdks").output();
    let dotnet10 = sdks.is_ok_and(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l.split('.').next().and_then(|m| m.parse::<u32>().ok()) >= Some(10))
    });
    dotnet10 && std::env::var_os("DGF_SKIP_E2E").is_none()
}
