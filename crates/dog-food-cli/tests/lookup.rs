//! `dgf outline`, `dgf show`, `dgf refs` and `dgf coverage` on a small
//! repository: several targets in one call, capped output that says what
//! it cut, and a target that names nothing reported next to the others.

use std::path::Path;
use std::process::Command;

const CART: &str = "\
namespace Shop;

public class Cart
{
    private readonly List<Item> _items = new();

    /// <summary>Adds an item.</summary>
    public void Add(Item item)
    {
        _items.Add(item);
    }

    public int Total(int discount)
    {
        if (discount > 0)
        {
            return _items.Count - discount;
        }
        return _items.Count;
    }
}
";

const ITEM: &str = "namespace Shop;\n\npublic record Item(string Name);\n";

const CART_TESTS: &str = "\
public class CartTests
{
    public void Adds()
    {
        new Cart().Add(new Item(\"a\"));
    }
}
";

const PRICE: &str = "\
export function price(amount: number): number {
  if (amount > 10) {
    return amount * 2;
  }
  return amount;
}
";

/// Lines 8 to 11 of Cart.cs ran; `Total` (13 to 20) ran with one of the
/// two branches of its `if`.
const LCOV: &str = "\
SF:src/Cart.cs
DA:8,1
DA:10,1
DA:13,1
DA:15,1
DA:17,0
DA:19,1
BRDA:15,0,0,0
BRDA:15,0,1,1
end_of_record
";

struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    /// `Cart.cs` and `Item.cs` on `main`; a branch that changes `Cart.cs`
    /// and adds a test and a TypeScript file.
    fn new() -> Repo {
        let repo = Repo {
            dir: tempfile::tempdir().unwrap(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.write(
            "src/Cart.cs",
            &CART.replace("    public int Total", "    int Total"),
        );
        repo.write("src/Item.cs", ITEM);
        repo.commit("base");
        repo.git(&["checkout", "-q", "-b", "feature"]);
        repo.write("src/Cart.cs", CART);
        repo.write("tests/CartTests.cs", CART_TESTS);
        repo.write("web/price.ts", PRICE);
        repo.commit("feature");
        repo.write("coverage/lcov.info", LCOV);
        repo
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, path: &str, text: &str) {
        let file = self.root().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "core.autocrlf=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// The exit code and what `dgf` printed.
    fn dgf(&self, args: &[&str]) -> (i32, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_dgf"))
            .args(args)
            .current_dir(self.root())
            .env_remove("DGF_PROGRESS")
            .output()
            .expect("dgf runs");
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        (output.status.code().unwrap_or(-1), text)
    }
}

#[test]
fn outline_lists_members_of_several_files_without_bodies() {
    let repo = Repo::new();
    let (code, text) = repo.dgf(&["outline", "src/Cart.cs", "web", "src/Missing.cs"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        text,
        "\
src/Cart.cs  (21 lines)
  1  namespace Shop
  3-21  public class Cart
    5  private readonly List<Item> _items = new()
    8-11  public void Add(Item item)
    13-20  public int Total(int discount)
web/price.ts  (6 lines)
  1-6  export function price(amount: number): number
src/Missing.cs: not found
"
    );
    let (_, public) = repo.dgf(&["outline", "src/*.cs", "--public", "--match", "Total|record"]);
    assert_eq!(
        public,
        "\
src/Cart.cs  (21 lines)
  3-21  public class Cart
    13-20  public int Total(int discount)
src/Item.cs  (3 lines)
  3  public record Item(string Name)
"
    );
}

#[test]
fn outline_of_the_changes_is_capped_and_says_what_it_cut() {
    let repo = Repo::new();
    let (code, text) = repo.dgf(&["outline", "--changes", "--base", "main", "--max-lines", "4"]);
    assert_eq!(code, 0, "{text}");
    assert!(
        text.starts_with("3 changed source files against main ("),
        "{text}"
    );
    // Item.cs did not change.
    assert!(!text.contains("Item.cs"), "{text}");
    assert!(text.contains("tests/CartTests.cs  (7 lines)"), "{text}");
    assert!(text.contains("web/price.ts  (6 lines)"), "{text}");
    assert!(
        text.contains("  ... 3 more declarations, narrow with --match or raise --max-lines"),
        "{text}"
    );
    let (none, text) = repo.dgf(&["outline", "nowhere"]);
    assert_eq!(none, 1, "{text}");
}

#[test]
fn show_prints_named_declarations_from_several_files() {
    let repo = Repo::new();
    let (code, text) = repo.dgf(&[
        "show",
        "src/Cart.cs:Add",
        "web/price.ts:3",
        "src/Item.cs:1-2",
        "src/Cart.cs:Totl",
        "Item",
    ]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        text,
        "\
src/Cart.cs:8-11  public void Add(Item item)
8\t    public void Add(Item item)
9\t    {
10\t        _items.Add(item);
11\t    }

web/price.ts:1-6  export function price(amount: number): number
1\texport function price(amount: number): number {
2\t  if (amount > 10) {
3\t    return amount * 2;
4\t  }
5\t  return amount;
6\t}

src/Item.cs:1-2
1\tnamespace Shop;
2\t

src/Cart.cs:Totl: not found; nearest: Total (13-20)
src/Item.cs:3  public record Item(string Name)
3\tpublic record Item(string Name);

"
    );
    let (_, signature) = repo.dgf(&["show", "src/Cart.cs:Cart.Add", "--signature"]);
    assert_eq!(
        signature,
        "\
src/Cart.cs:8-11  public void Add(Item item)
7\t    /// <summary>Adds an item.</summary>
8\t    public void Add(Item item)

"
    );
    let (none, text) = repo.dgf(&["show", "src/Cart.cs:Nothing", "Zebra"]);
    assert_eq!(none, 1, "{text}");
    assert!(
        text.contains("Zebra: no declaration with this name under ."),
        "{text}"
    );
}

#[test]
fn refs_lists_uses_by_file_with_tests_last() {
    let repo = Repo::new();
    let (code, text) = repo.dgf(&["refs", "Add", "Cart.Total", "Zebra"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        text,
        "\
refs: by name from the syntax tree, not resolved by type: another declaration with the same name is listed too
Add: 3 lines in 2 files, declared on 1 of them
  src/Cart.cs
        8  def  Cart  public void Add(Item item)
       10       Cart.Add  _items.Add(item);
  tests:
  tests/CartTests.cs
        5       CartTests.Adds  new Cart().Add(new Item(\"a\"));
Total (of Cart.Total): 1 lines in 1 files, declared on 1 of them
  src/Cart.cs
       13  def  Cart  public int Total(int discount)
Zebra: no uses under .; names are matched whole and with their case
"
    );
    let (_, narrowed) = repo.dgf(&["refs", "Item", "--in", "tests"]);
    assert!(
        narrowed.contains("Item: 1 lines in 1 files, declared on 0 of them"),
        "{narrowed}"
    );
    let (none, _) = repo.dgf(&["refs", "Zebra"]);
    assert_eq!(none, 1);
}

#[test]
fn coverage_of_the_changes_fails_below_the_floor() {
    let repo = Repo::new();
    let (code, text) = repo.dgf(&[
        "coverage",
        "coverage",
        "--changes",
        "--base",
        "main",
        "--floor",
        "90",
    ]);
    assert_eq!(code, 1, "{text}");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with("coverage: 2 changed source files against main (")
            && lines[0].ends_with(") (1 changed test files left out), 1 reports"),
        "{text}"
    );
    assert_eq!(
        lines[1..],
        [
            "  line%  branch%  file",
            "   83.3     50.0  src/Cart.cs  BELOW 90",
            "                    not run: 17",
            "                    branches partly taken: 15 (1/2)",
            "      -        -  web/price.ts  not in the reports  BELOW 90",
            "total: 83.3% of lines (5/6), 50.0% of branches (1/2); 1 files with functions are not in the reports (no test ran them, or their project was not measured) and are not counted",
            "floor 90: fail, 2 files are below it on lines or branches",
        ],
        "{text}"
    );
}

#[test]
fn coverage_per_function_of_every_file_in_the_reports() {
    let repo = Repo::new();
    let (code, text) = repo.dgf(&["coverage", "coverage", "--functions", "--floor", "50"]);
    assert_eq!(code, 0, "{text}");
    assert_eq!(
        text,
        "\
coverage: 1 source files in the reports, 1 reports
  line%  branch%  file
   83.3     50.0  src/Cart.cs
  100.0        -    Cart.Add (8-11)
   75.0     50.0    Cart.Total (13-20)
                    not run: 17
                    branches partly taken: 15 (1/2)
total: 83.3% of lines (5/6), 50.0% of branches (1/2)
floor 50: pass, every file is at or above it on lines and branches
"
    );
    let (usage, _) = repo.dgf(&["coverage", "--changes"]);
    assert_eq!(usage, 2);
}
