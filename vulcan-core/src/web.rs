use html_to_markdown_rs::{convert, ConversionOptions, HeadingStyle, ListIndentType};
use readabilityrs::{is_probably_readerable, Readability};
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebFetchExtractionMode {
    #[default]
    Auto,
    Article,
    Generic,
}

impl WebFetchExtractionMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Article => "article",
            Self::Generic => "generic",
        }
    }
}

#[must_use]
pub fn html_to_markdown(
    html: &str,
    url: Option<&str>,
    extraction_mode: WebFetchExtractionMode,
) -> String {
    match extraction_mode {
        WebFetchExtractionMode::Auto => {
            if should_extract_article(html) {
                article_html_to_markdown(html, url)
                    .unwrap_or_else(|| generic_html_to_markdown(html))
            } else {
                generic_html_to_markdown(html)
            }
        }
        WebFetchExtractionMode::Article => {
            article_html_to_markdown(html, url).unwrap_or_else(|| generic_html_to_markdown(html))
        }
        WebFetchExtractionMode::Generic => generic_html_to_markdown(html),
    }
}

fn should_extract_article(html: &str) -> bool {
    is_probably_readerable(html, None)
        || Regex::new(r"(?i)<article(?:\s|>)")
            .expect("regex should compile")
            .is_match(html)
}

fn article_html_to_markdown(html: &str, url: Option<&str>) -> Option<String> {
    let readability = Readability::new(html, url, None).ok()?;
    let article = readability.parse()?;
    article
        .content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .map(generic_html_to_markdown)
        .or_else(|| {
            article
                .text_content
                .as_deref()
                .filter(|content| !content.trim().is_empty())
                .map(normalize_plain_text)
        })
}

fn generic_html_to_markdown(html: &str) -> String {
    convert(html, Some(default_conversion_options())).map_or_else(
        |_| legacy_html_to_markdown(html),
        |markdown| markdown.trim().to_string(),
    )
}

fn default_conversion_options() -> ConversionOptions {
    ConversionOptions {
        heading_style: HeadingStyle::Atx,
        list_indent_type: ListIndentType::Spaces,
        list_indent_width: 2,
        bullets: "-".to_string(),
        ..ConversionOptions::default()
    }
}

fn normalize_plain_text(content: &str) -> String {
    Regex::new(r"\n{3,}")
        .expect("regex should compile")
        .replace_all(content.trim(), "\n\n")
        .into_owned()
}

fn legacy_html_to_markdown(html: &str) -> String {
    let mut rendered = Regex::new(r"(?is)<script[^>]*>.*?</script>")
        .expect("regex should compile")
        .replace_all(html, "")
        .into_owned();
    rendered = Regex::new(r"(?is)<style[^>]*>.*?</style>")
        .expect("regex should compile")
        .replace_all(&rendered, "")
        .into_owned();
    for (pattern, replacement) in [
        (r"(?i)<br\s*/?>", "\n"),
        (
            r"(?i)</(p|div|section|article|main|body|h1|h2|h3|h4|h5|h6|tr)>",
            "\n",
        ),
        (r"(?i)<li[^>]*>", "- "),
        (r"(?i)</li>", "\n"),
    ] {
        rendered = Regex::new(pattern)
            .expect("regex should compile")
            .replace_all(&rendered, replacement)
            .into_owned();
    }
    rendered = Regex::new(r"(?is)<[^>]+>")
        .expect("regex should compile")
        .replace_all(&rendered, "")
        .into_owned();
    rendered = decode_html_entities(&rendered);
    Regex::new(r"\n{3,}")
        .expect("regex should compile")
        .replace_all(rendered.trim(), "\n\n")
        .into_owned()
}

fn decode_html_entities(input: &str) -> String {
    [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
    ]
    .into_iter()
    .fold(input.to_string(), |acc, (from, to)| acc.replace(from, to))
}

#[cfg(test)]
mod tests {
    use super::{html_to_markdown, WebFetchExtractionMode};

    #[test]
    fn auto_mode_prefers_article_extraction_for_readerable_pages() {
        let html = r#"<!doctype html><html><body>
<nav>skip me</nav>
<article>
  <h1>Release Summary</h1>
  <p>This is a substantial article paragraph with enough detail to cross the readerability threshold and keep the extraction path focused on the main content instead of the surrounding chrome.</p>
</article>
</body></html>"#;

        let markdown = html_to_markdown(
            html,
            Some("https://example.com/release"),
            WebFetchExtractionMode::Auto,
        );
        assert!(markdown.contains("Release Summary"));
        assert!(markdown.contains("substantial article paragraph"));
        assert!(!markdown.contains("skip me"));
    }

    #[test]
    fn generic_mode_keeps_non_article_page_chrome() {
        let html = r#"<!doctype html><html><body>
<nav>Site Nav</nav>
<main><h1>Docs</h1><p>Short</p></main>
</body></html>"#;

        let markdown = html_to_markdown(
            html,
            Some("https://example.com/docs"),
            WebFetchExtractionMode::Generic,
        );
        assert!(markdown.contains("Site Nav"));
        assert!(markdown.contains("Docs"));
        assert!(markdown.contains("Short"));
    }

    #[test]
    fn auto_mode_falls_back_to_generic_for_non_readerable_pages() {
        let html = r#"<!doctype html><html><body>
<nav>Site Nav</nav>
<main><h1>Docs</h1><p>Short</p></main>
</body></html>"#;

        let auto = html_to_markdown(
            html,
            Some("https://example.com/docs"),
            WebFetchExtractionMode::Auto,
        );
        let generic = html_to_markdown(
            html,
            Some("https://example.com/docs"),
            WebFetchExtractionMode::Generic,
        );
        assert_eq!(auto, generic);
    }
}
