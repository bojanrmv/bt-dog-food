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
    let classes = dog_food_dup::semantic::classes(command, &paths, corpus, ask()).unwrap();
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
        dog_food_dup::semantic::classes(command, &[root.to_path_buf()], root, ask()).unwrap();
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
        [
            "OpenOrders.List",
            "OrderCounts.Count",
            "OrderLookup.First",
            "StoreSummaries.List"
        ]
    );
    // One of them is a correlated subquery, where EF Core cannot call the extension.
    assert!(
        filter
            .cautions
            .iter()
            .any(|c| c.contains("1 of these sits inside another query's lambda")),
        "{:?}",
        filter.cautions
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
    assert_eq!(find("NotDeleted").copies.len(), 5);
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

/// What the engine is asked in these tests: the corpus is under `tests/`.
fn ask() -> dog_food_dup::semantic::Ask {
    dog_food_dup::semantic::Ask {
        tests: true,
        confirm: false,
        regions: false,
    }
}

#[test]
fn running_them_confirms_every_rewrite_group_in_the_corpus() {
    if !has_dotnet10() {
        eprintln!("skipped: needs a .NET 10 SDK");
        return;
    }
    let corpus = Path::new(CORPUS);
    let paths = [corpus.join("Cases"), corpus.join("Model.cs")];
    let command = dog_food_dup::semantic::command(Some(&engine()));
    let ask = dog_food_dup::semantic::Ask {
        confirm: true,
        ..ask()
    };
    let classes = dog_food_dup::semantic::classes(command, &paths, corpus, ask).unwrap();
    let groups: Vec<_> = classes.iter().filter(|c| c.clone_type == 4).collect();
    assert!(groups.len() >= 16, "{}", groups.len());
    for group in groups {
        let confirmation = group.confirmation.as_ref().expect("a confirmation");
        assert_eq!(confirmation.status, "confirmed", "{:?}", group.copies);
        assert_eq!(confirmation.tries, 1000);
    }
}

#[test]
fn running_them_tells_apart_every_near_miss_it_can_run() {
    if !has_dotnet10() {
        eprintln!("skipped: needs a .NET 10 SDK");
        return;
    }
    let corpus = Path::new(CORPUS);
    let labels: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(corpus.join("cases.json")).unwrap()).unwrap();
    let mut disproved = 0;
    for pair in labels["pairs"].as_array().unwrap() {
        let (a, b) = (pair["a"].as_str().unwrap(), pair["b"].as_str().unwrap());
        let mut command = dog_food_dup::semantic::command(Some(&engine()));
        let output = command
            .arg(corpus)
            .args(["--tests", "--compare", a, b])
            .output()
            .unwrap();
        let verdict: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let status = verdict["status"].as_str().unwrap();
        if pair["equal"].as_bool().unwrap() {
            assert_eq!(status, "confirmed", "{a} vs {b}: {verdict}");
            assert_eq!(output.status.code(), Some(0));
            continue;
        }
        // A near miss is never confirmed; it is told apart, or not run (with effects, say).
        assert_ne!(status, "confirmed", "{a} vs {b}: {verdict}");
        if status == "disproved" {
            disproved += 1;
            assert_eq!(output.status.code(), Some(1));
            assert!(verdict["example"].as_str().unwrap().contains(" vs "));
        }
    }
    assert!(disproved >= 12, "{disproved}");
}

const CONFIRM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/confirm");

#[test]
fn methods_are_only_run_when_they_have_no_effects() {
    if !has_dotnet10() {
        eprintln!("skipped: needs a .NET 10 SDK");
        return;
    }
    let root = Path::new(CONFIRM);
    let command = dog_food_dup::semantic::command(Some(&engine()));
    let ask = dog_food_dup::semantic::Ask {
        tests: true,
        confirm: true,
        regions: false,
    };
    let classes =
        dog_food_dup::semantic::classes(command, &[root.to_path_buf()], root, ask).unwrap();
    let status = |function: &str| {
        let class = classes
            .iter()
            .find(|c| {
                c.copies
                    .iter()
                    .any(|x| x.function.as_deref() == Some(function))
            })
            .unwrap_or_else(|| panic!("no group with {function}"));
        class.confirmation.clone().unwrap()
    };
    // A switch and ifs building one record; a condition in a constructor's argument.
    assert_eq!(status("Badges.FromSwitch").status, "confirmed");
    assert_eq!(status("Prices.Discounted").status, "confirmed");
    // These read the clock and write a file.
    let stamps = status("Stamps.Today");
    assert_eq!(stamps.status, "not-run");
    assert!(
        stamps.reason.as_deref().unwrap().contains("DateTime.Now"),
        "{stamps:?}"
    );
}

const REGIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/regions");

#[test]
fn a_csharp_copy_gets_its_parameters_and_results_from_the_compiler() {
    if !has_dotnet10() {
        eprintln!("skipped: needs a .NET 10 SDK");
        return;
    }
    let root = Path::new(REGIONS);
    let paths = [root.to_path_buf()];
    let settings = Settings {
        min_tokens: 30,
        min_lines: 4,
    };
    let command = dog_food_dup::semantic::command(Some(&engine()));
    let ask = dog_food_dup::semantic::Ask {
        regions: true,
        ..ask()
    };
    let engine = dog_food_dup::semantic::start(command, &paths, root, ask).unwrap();
    let mut found = scan(&paths, root, true, settings);
    dog_food_dup::semantic::finish_with(engine, &mut found).unwrap();
    let class = found
        .classes
        .iter()
        .find(|c| c.signature.is_some())
        .unwrap_or_else(|| panic!("{:#?}", found.classes));
    let signature = class.signature.as_ref().unwrap();
    // The copy runs from the heading to the rounding (the strings and calls
    // that differ are holes): it reads the parameters, and the return after
    // it reads the heading and the rounded total.
    assert_eq!(
        signature.parameters,
        ["IReadOnlyList<Line> lines", "string customer"],
        "{signature:?}"
    );
    assert!(
        signature.returns.iter().any(|r| r == "decimal rounded"),
        "{signature:?}"
    );
    assert!(
        signature.returns.iter().any(|r| r.ends_with(" heading")),
        "{signature:?}"
    );
    assert!(signature.exits.is_empty(), "{signature:?}");
    // It reads the field `fee`.
    assert!(signature.uses_this, "{signature:?}");
    let text = report::table(&found, 0);
    assert!(
        text.contains("as a method (from the compiler): takes ("),
        "{text}"
    );
}
