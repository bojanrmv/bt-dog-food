//! The `dgf analyze --format html` page: the JSON report embedded in a
//! template that draws the charts in the browser. No network access needed.

use super::page::page;
use super::{json, Run};

const TEMPLATE: &str = include_str!("template.html");

pub fn render(run: &Run) -> String {
    page(TEMPLATE, &json(run))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::page::tests::assert_slots;
    use crate::report::tests::{file, function, limits, run};

    #[test]
    fn template_has_every_slot_once() {
        assert_slots(TEMPLATE);
    }

    #[test]
    fn page_embeds_the_report_data() {
        let report = file(vec![function(
            "OrderService.CalculateDiscount",
            8,
            &[("if", 7)],
        )]);
        let limits = limits(5, 6);
        let page = render(&run(&report, &limits));
        assert!(page.contains(r#""name":"OrderService.CalculateDiscount""#));
        assert!(page.contains(r#""fail":1"#));
    }
}
