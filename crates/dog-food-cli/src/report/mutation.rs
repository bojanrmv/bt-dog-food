//! The `dgf mutate --run --format html` page.

use super::page::page;

const TEMPLATE: &str = include_str!("mutation.html");

pub fn render(data: &serde_json::Value) -> String {
    page(TEMPLATE, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::page::tests::assert_slots;

    #[test]
    fn template_has_every_slot_once() {
        assert_slots(TEMPLATE);
        assert!(render(&serde_json::json!({ "score": 50.0 })).contains(r#"{"score":50.0}"#));
    }
}
