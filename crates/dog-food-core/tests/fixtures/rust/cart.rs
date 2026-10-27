// Fixture: a `dgf: N` comment states the expected complexity of the function
// that starts on the next line. This file is test data, not compiled.

// dgf: 1
fn add(a: i32, b: i32) -> i32 {
    a + b
}

// dgf: 4 cognitive=4 nesting=1 params=1
fn classify(x: i32) -> i32 {
    if x < 0 {
        -1
    } else if x == 0 {
        0
    } else if x > 100 {
        2
    } else {
        1
    }
}

// dgf: 3
fn describe(n: Option<u32>) -> &'static str {
    match n {
        Some(0) => "zero",
        Some(_) => "some",
        None => "none",
    }
}

struct Cart {
    items: Vec<u32>,
}

impl Cart {
    // dgf: 4 cognitive=4 nesting=2 params=2
    fn total(&self, limit: u32) -> u32 {
        let mut sum = 0;
        for item in &self.items {
            if *item > limit || *item == 0 {
                continue;
            }
            sum += item;
        }
        sum
    }
}

// dgf: 2
fn evens(v: &[i32]) -> Vec<i32> {
    if v.is_empty() {
        return vec![];
    }
    v.iter().copied().filter(|x| x % 2 == 0).collect()
}
