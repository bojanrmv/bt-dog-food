//! Assembles a self-contained HTML page: shared stylesheet and script, the
//! page's own template, and the report data as JSON.

const STYLE: &str = include_str!("style.css");
const COMMON: &str = include_str!("common.js");
const STYLE_SLOT: &str = "/*DGF_STYLE*/";
const COMMON_SLOT: &str = "/*DGF_COMMON*/";
pub const DATA_SLOT: &str = "/*DGF_DATA*/null";

pub fn page(template: &str, data: &serde_json::Value) -> String {
    let data = serde_json::to_string(data).expect("report serializes");
    // `</script>` inside a function name or path would end the script early.
    let data = data.replace("</", r"<\/");
    // Data goes in last, so text inside it is never mistaken for a slot.
    template
        .replacen(STYLE_SLOT, STYLE, 1)
        .replacen(COMMON_SLOT, COMMON, 1)
        .replacen(DATA_SLOT, &data, 1)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Every page template has each slot exactly once.
    pub fn assert_slots(template: &str) {
        for slot in [STYLE_SLOT, COMMON_SLOT, DATA_SLOT] {
            assert_eq!(template.matches(slot).count(), 1, "{slot}");
        }
    }

    /// `Math.max(...list)` passes every item as an argument, and a call takes
    /// only so many, so a page must work for a list of any length.
    #[test]
    fn no_page_spreads_a_list_into_max_or_min() {
        let pages = [
            ("template.html", include_str!("template.html")),
            ("changes.html", include_str!("changes.html")),
            ("history.html", include_str!("history.html")),
            ("mutation.html", include_str!("mutation.html")),
            ("common.js", COMMON),
        ];
        for (name, text) in pages {
            let spread = |line: &&str| {
                (line.contains("Math.max(") || line.contains("Math.min(")) && line.contains("...")
            };
            let found: Vec<&str> = text.lines().filter(spread).collect();
            assert!(found.is_empty(), "{name}: {found:?}");
        }
    }

    #[test]
    fn fills_every_slot_and_escapes_script_ends() {
        let template = "<style>/*DGF_STYLE*/</style><script>/*DGF_COMMON*/\nconst D = /*DGF_DATA*/null;</script>";
        let page = page(
            template,
            &serde_json::json!({ "name": "</script>/*DGF_STYLE*/" }),
        );
        assert!(page.contains(":root"));
        assert!(page.contains("function niceTicks"));
        assert!(page.contains(r#"{"name":"<\/script>/*DGF_STYLE*/"}"#));
        assert_eq!(page.matches("</script>").count(), 1);
    }
}
