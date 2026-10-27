//! Prints a file with every mutant switched in, as `dgf mutate --run`
//! compiles it: `cargo run -p dog-food-core --example schemata -- File.cs`.
//! The switch is `Dgf.Is(id)`; a test project supplies that class.

use dog_food_core::mutate::{mutate, schemata, Level, Settings};
use dog_food_core::{Language, Mutant};

fn main() {
    let path = std::env::args().nth(1).expect("usage: schemata <file>");
    let source = std::fs::read_to_string(&path).expect("readable file");
    let language = Language::from_path(std::path::Path::new(&path)).expect("a known language");
    let settings = Settings {
        level: Level::Advanced,
        ..Settings::default()
    };
    let found = mutate(&source, language, &settings).expect("mutants");
    let active: Vec<&Mutant> = found
        .mutants
        .iter()
        .filter(|m| m.ignored.is_none())
        .collect();
    print!(
        "{}",
        schemata(&source, language, &active, &|id| format!("Dgf.Is({id})"))
    );
}
