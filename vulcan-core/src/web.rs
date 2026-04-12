use regex::Regex;
use rs_trafilatura::{extract_with_options, Options};

pub fn html_to_markdown(html: &str, url: Option<&str>) -> Result<String, String> {
    extract_markdown(html, url)
}

fn extract_markdown(html: &str, url: Option<&str>) -> Result<String, String> {
    let options = Options {
        output_markdown: true,
        include_links: true,
        include_tables: true,
        favor_precision: true,
        url: url.map(ToOwned::to_owned),
        ..Options::default()
    };
    let result = extract_with_options(html, &options).map_err(|error| {
        format!(
            "rs-trafilatura extraction failed: {error}; retry with HTML or raw output if you need the original page"
        )
    })?;
    result
        .content_markdown
        .as_deref()
        .filter(|content| !content.trim().is_empty())
        .map(normalize_markdown)
        .or_else(|| {
            (!result.content_text.trim().is_empty())
                .then(|| normalize_plain_text(&result.content_text))
        })
        .ok_or_else(|| {
            "rs-trafilatura could not extract readable main content from the fetched HTML; retry with HTML or raw output if you need the original page".to_string()
        })
}

fn normalize_markdown(content: &str) -> String {
    Regex::new(r"\n{3,}")
        .expect("regex should compile")
        .replace_all(content.trim(), "\n\n")
        .into_owned()
}

fn normalize_plain_text(content: &str) -> String {
    Regex::new(r"\n{3,}")
        .expect("regex should compile")
        .replace_all(content.trim(), "\n\n")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::html_to_markdown;

    #[test]
    fn extracts_main_content_markdown_for_article_pages() {
        let html = r"<!doctype html><html><body>
<nav>skip me</nav>
<article>
  <h1>Release Summary</h1>
  <p>This is a substantial article paragraph with enough detail to cross the extraction confidence threshold and keep the extraction path focused on the main content instead of the surrounding chrome.</p>
</article>
</body></html>";

        let markdown =
            html_to_markdown(html, Some("https://example.com/release")).expect("should extract");
        assert!(markdown.contains("Release Summary"));
        assert!(markdown.contains("substantial article paragraph"));
        assert!(!markdown.contains("skip me"));
    }

    #[test]
    fn strips_page_chrome_when_extracting_docs_content() {
        let html = r"<!doctype html><html><body>
<nav>Site Nav</nav>
<main><h1>Docs</h1><p>Short</p></main>
</body></html>";

        let markdown =
            html_to_markdown(html, Some("https://example.com/docs")).expect("should extract");
        assert!(!markdown.contains("Site Nav"));
        assert!(markdown.contains("Docs"));
        assert!(markdown.contains("Short"));
    }

    #[test]
    fn errors_when_no_readable_main_content_is_found() {
        let html = "<!doctype html><html><body></body></html>";

        let error = html_to_markdown(html, Some("https://example.com/empty"))
            .expect_err("empty pages should not produce markdown");
        assert!(error.contains("could not extract readable main content"));
    }
}
