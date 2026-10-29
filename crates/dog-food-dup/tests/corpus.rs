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
    let paths = [corpus.join("Cases"), corpus.join("Model.cs")];
    let mut found = scan(&paths, corpus, true, Settings::default());
    let command = dog_food_dup::semantic::command(Some(&engine()));
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

/// The C# engine, built once for all the tests that need it.
fn engine() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let artifacts = Path::new(env!("CARGO_TARGET_TMPDIR")).join("dgf-dup-cs");
            let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("semantic/DgfDupCs.csproj");
            let built = Command::new("dotnet")
                .args(["build", "-c", "Release", "-o"])
                .arg(&artifacts)
                .arg(&project)
                .output()
                .unwrap();
            assert!(
                built.status.success(),
                "{}",
                String::from_utf8_lossy(&built.stdout)
            );
            artifacts.join("dgf-dup-cs.dll")
        })
        .clone()
}

const QUERIES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/queries");

#[test]
fn repeated_queries_on_a_table_get_one_extension_method_each() {
    if !has_dotnet10() {
        eprintln!("skipped: needs a .NET 10 SDK");
        return;
    }
    let root = Path::new(QUERIES);
    let command = dog_food_dup::semantic::command(Some(&engine()));
    let classes =
        dog_food_dup::semantic::classes(command, &[root.to_path_buf()], root, true).unwrap();
    let queries: Vec<_> = classes.iter().filter(|c| c.kind == "query").collect();
    let functions = |c: &dog_food_dup::CloneClass| {
        let mut f: Vec<String> = c.copies.iter().filter_map(|x| x.function.clone()).collect();
        f.sort();
        f
    };
    let find = |name: &str| {
        queries
            .iter()
            .find(|c| c.template.contains(&format!(" {name}(")))
            .unwrap_or_else(|| panic!("no {name} in {queries:#?}"))
    };
    // The same filter, split, reordered, renamed, or as FirstOrDefault's predicate.
    let filter = find("ForShopNotDeleted");
    assert_eq!(
        functions(filter),
        ["OpenOrders.List", "OrderCounts.Count", "OrderLookup.First"]
    );
    assert_eq!(filter.entity.as_deref(), Some("Shop.Order"));
    assert!(
        filter.template.contains("Guid shopId"),
        "{}",
        filter.template
    );
    assert!(
        filter
            .template
            .contains("query.Where(o => o.ShopId == shopId && !o.IsDeleted)"),
        "{}",
        filter.template
    );
    // The soft-delete condition, in four different queries.
    assert_eq!(find("NotDeleted").copies.len(), 4);
    // The same filter, order and projection.
    let shaped = find("ForShopSelected");
    assert_eq!(
        functions(shaped),
        ["RecentTotals.Totals", "TotalsReport.Latest"]
    );
    assert!(shaped
        .template
        .starts_with("public static IQueryable<decimal>"));
    // The deleted orders are another filter.
    assert!(!functions(filter).contains(&"DeletedOrders.List".to_string()));
}
