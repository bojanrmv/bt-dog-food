//! Planted copies: what is found, the template and holes, and how to unite.

use std::path::{Path, PathBuf};

use dog_food_dup::{scan, HoleKind, Report, Settings};

/// A copy body, with the names that differ between its copies.
fn body(item: &str, price: &str, total: &str) -> String {
    format!(
        "        var {total} = 0m;
        if ({item}.Active && {item}.End > now)
        {{
            {total} += {item}.{price} * {item}.Quantity;
            log.Add($\"{{{item}.Id}} counted\");
        }}
        if ({total} > limit)
        {{
            {total} = limit;
        }}
        return {total};
"
    )
}

fn write(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

fn settings() -> Settings {
    Settings {
        min_tokens: 30,
        min_lines: 4,
    }
}

fn run(root: &Path, tests: bool) -> Report {
    scan(&[root.to_path_buf()], root, tests, settings())
}

#[test]
fn renamed_copies_become_one_template_with_holes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/Billing.cs",
        &format!(
            "class Orders {{\n    decimal Add(Plan plan, decimal limit) {{\n{}    }}\n}}\n\nclass Seats {{\n    decimal Add(Seat seat, decimal limit) {{\n{}    }}\n}}\n",
            body("plan", "Price", "total"),
            body("seat", "Fee", "sum")
        ),
    );
    let report = run(root, false);
    assert_eq!(report.classes.len(), 1, "{report:#?}");
    let class = &report.classes[0];
    assert_eq!(class.kind, "copies");
    assert_eq!(class.copies.len(), 2);
    assert_eq!(class.copies[0].file, "src/Billing.cs");
    assert_eq!(class.copies[0].function.as_deref(), Some("Orders.Add"));
    assert_eq!(class.copies[1].function.as_deref(), Some("Seats.Add"));
    let kind_of = |value: &str| {
        class
            .holes
            .iter()
            .find(|h| h.values.iter().any(|v| v == value))
            .map(|h| h.kind)
    };
    assert_eq!(kind_of("Fee"), Some(HoleKind::Member));
    assert_eq!(kind_of("seat"), Some(HoleKind::Name));
    // `total` / `sum` is a local the copy declares: renamed, not a parameter.
    assert_eq!(kind_of("sum"), Some(HoleKind::Local));
    assert!(class.template.contains("⟨"), "{}", class.template);
    // Both bodies are copies, start to end.
    assert!(class.unite.contains("keep one function"), "{}", class.unite);
    assert!(class.unite.contains("selector"), "{}", class.unite);
    // Kept as one function, its return stays as it is.
    assert!(class.cautions.is_empty(), "{:?}", class.cautions);
}

#[test]
fn copies_inside_bigger_functions_are_extracted_with_their_cautions() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/Billing.cs",
        &format!(
            "class Orders {{\n    decimal Add(Plan plan, decimal limit) {{\n        Audit(plan.Id);\n{}    }}\n}}\n\nclass Seats {{\n    decimal Add(Seat seat, decimal limit) {{\n        Trace(seat.Id, 2);\n        Check(seat);\n{}    }}\n}}\n",
            body("plan", "Price", "total"),
            body("seat", "Fee", "sum")
        ),
    );
    let report = run(root, false);
    assert_eq!(report.classes.len(), 1, "{report:#?}");
    let class = &report.classes[0];
    assert!(
        class.unite.contains("shared function in this file"),
        "{}",
        class.unite
    );
    assert!(
        class.cautions.iter().any(|c| c.contains("return")),
        "{:?}",
        class.cautions
    );
}

#[test]
fn the_html_report_carries_the_findings_and_cannot_be_broken_out_of() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A string in the code that would end the page's script if pasted as is.
    let sneaky = body("plan", "Price", "total").replace("counted", "</script><b>x");
    write(
        root,
        "src/Billing.cs",
        &format!(
            "class Orders {{\n    decimal Add(Plan plan, decimal limit) {{\n{sneaky}    }}\n}}\n\nclass Seats {{\n    decimal Add(Seat seat, decimal limit) {{\n{}    }}\n}}\n",
            body("seat", "Fee", "sum").replace("counted", "</script><b>x")
        ),
    );
    let report = run(root, false);
    assert_eq!(report.classes.len(), 1);
    assert_eq!(report.file_lines.get("src/Billing.cs"), Some(&31));
    let page = dog_food_dup::report::html(&report);
    assert!(
        page.contains("\"file\":\"src/Billing.cs\""),
        "the data is in the page"
    );
    assert_eq!(
        page.matches("</script>").count(),
        1,
        "only the page's own script ends"
    );
}

#[test]
fn a_different_operator_is_not_a_copy() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let other = body("seat", "Fee", "sum").replace("* seat.Quantity", "- seat.Quantity");
    write(
        root,
        "src/Billing.cs",
        &format!(
            "class Orders {{\n    decimal Add(Plan plan, decimal limit) {{\n{}    }}\n}}\n\nclass Seats {{\n    decimal Add(Seat seat, decimal limit) {{\n{other}    }}\n}}\n",
            body("plan", "Price", "total"),
        ),
    );
    let report = run(root, false);
    assert!(
        report.classes.iter().all(|c| c.statements < 3),
        "only the parts before the change may match: {report:#?}"
    );
}

#[test]
fn statements_repeated_in_a_row_are_one_finding() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let fields = ["firstName", "lastName", "phone", "city", "country"];
    let assignments: String = fields
        .iter()
        .map(|f| format!("  if (dto.{f} !== undefined) {{\n    user.{f} = dto.{f};\n  }}\n"))
        .collect();
    write(
        root,
        "web/user.ts",
        &format!("export function update(user: User, dto: Dto) {{\n{assignments}}}\n"),
    );
    let report = run(root, false);
    let repeated: Vec<_> = report
        .classes
        .iter()
        .filter(|c| c.kind == "repeated")
        .collect();
    assert_eq!(repeated.len(), 1, "{report:#?}");
    assert_eq!(repeated[0].copies.len(), 5);
    assert!(
        repeated[0].unite.contains("loop over it"),
        "{}",
        repeated[0].unite
    );
    assert_eq!(repeated[0].holes[0].values, fields);
    assert!(
        report.classes.iter().all(|c| c.kind == "repeated"),
        "no overlapping copies of the same run: {report:#?}"
    );
}

#[test]
fn test_code_is_left_out_unless_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "tests/BillingTests.cs",
        &format!(
            "class A {{\n    decimal F(Plan plan, decimal limit) {{\n{}    }}\n    decimal G(Plan plan, decimal limit) {{\n{}    }}\n}}\n",
            body("plan", "Price", "total"),
            body("plan", "Fee", "total")
        ),
    );
    assert!(run(root, false).classes.is_empty());
    let with_tests = run(root, true);
    assert_eq!(with_tests.classes.len(), 1, "{with_tests:#?}");
    assert!(
        with_tests.classes[0].unite.contains("keep one function"),
        "whole bodies: {}",
        with_tests.classes[0].unite
    );
}
