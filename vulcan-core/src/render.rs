use pulldown_cmark::{html, Parser};

use crate::parser::{fragment_parser_options, parser_options};

#[must_use]
pub fn render_markdown_html(source: &str) -> String {
    render_markdown_html_with_options(source, parser_options())
}

#[must_use]
pub fn render_markdown_fragment_html(source: &str) -> String {
    render_markdown_html_with_options(source, fragment_parser_options())
}

fn render_markdown_html_with_options(source: &str, options: pulldown_cmark::Options) -> String {
    let parser = Parser::new_ext(source, options);
    let mut rendered = String::new();
    html::push_html(&mut rendered, parser);
    rendered
}

#[cfg(test)]
mod tests {
    use super::{render_markdown_fragment_html, render_markdown_html};

    #[test]
    fn full_document_render_omits_frontmatter_from_html() {
        let html = render_markdown_html("---\nstatus: draft\n---\n\n# Dashboard\n\nAlpha");
        assert!(html.contains("<h1>Dashboard</h1>"));
        assert!(html.contains("<p>Alpha</p>"));
        assert!(!html.contains("status: draft"));
    }

    #[test]
    fn fragment_render_keeps_leading_thematic_breaks() {
        let html = render_markdown_fragment_html("---\n\nAfter");
        assert!(html.contains("<hr />"));
        assert!(html.contains("<p>After</p>"));
    }
}
