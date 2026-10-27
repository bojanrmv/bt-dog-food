fn main() {
    let path = std::env::args().nth(1).expect("usage: sites <file>");
    let source = std::fs::read_to_string(&path).unwrap();
    let language = dog_food_core::Language::from_path(std::path::Path::new(&path)).unwrap();
    for m in dog_food_core::mutate::mutants(&source, language).unwrap() {
        println!(
            "{} {:?} -> {:?} site {:?} multiline {}",
            m.mutator,
            m.original,
            m.replacement,
            &source[m.site_start..m.site_end],
            m.multiline
        );
    }
}
