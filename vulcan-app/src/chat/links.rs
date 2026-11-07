//! A message that is only a web link is captured as `[Page title](url)`.

use super::ChatVault;
use regex::Regex;
use vulcan_core::load_vault_config;

/// The most of a page read to find its title.
#[cfg(feature = "web")]
const TITLE_READ_LIMIT: u64 = 512 * 1024;
/// Redirects followed, each checked against the network permission.
#[cfg(feature = "web")]
const MAX_REDIRECTS: usize = 3;
const MAX_TITLE_CHARS: usize = 200;

/// `[Page title](url)` when `text` is one http(s) link, `bot.link_titles`
/// is on, the user's profile may reach the page, and it has a title. `None`
/// otherwise, so the text is captured unchanged.
pub(super) fn link_with_title(vault: &ChatVault<'_>, text: &str) -> Option<String> {
    let url = text.trim();
    if url.contains(char::is_whitespace)
        || !(url.starts_with("https://") || url.starts_with("http://"))
    {
        return None;
    }
    let config = load_vault_config(vault.paths).config;
    if !config.bot.link_titles {
        return None;
    }
    let html = fetch_html(vault, url, &config.web.user_agent)?;
    let title = html_title(&html)?;
    Some(format!("[{}]({url})", escape_link_text(&title)))
}

#[cfg(feature = "web")]
fn fetch_html(vault: &ChatVault<'_>, url: &str, user_agent: &str) -> Option<String> {
    use std::io::Read;
    use vulcan_core::PermissionGuard;

    let client = reqwest::blocking::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let mut current = reqwest::Url::parse(url).ok()?;
    for _ in 0..=MAX_REDIRECTS {
        vault.guard.check_network(current.as_str()).ok()?;
        let response = client.get(current.clone()).send().ok()?;
        if response.status().is_redirection() {
            let location = response.headers().get(reqwest::header::LOCATION)?;
            current = current.join(location.to_str().ok()?).ok()?;
            if !matches!(current.scheme(), "http" | "https") {
                return None;
            }
            continue;
        }
        let html = response.status().is_success()
            && response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("html"));
        if !html {
            return None;
        }
        let mut bytes = Vec::new();
        response
            .take(TITLE_READ_LIMIT)
            .read_to_end(&mut bytes)
            .ok()?;
        return Some(String::from_utf8_lossy(&bytes).into_owned());
    }
    None
}

#[cfg(not(feature = "web"))]
fn fetch_html(_vault: &ChatVault<'_>, _url: &str, _user_agent: &str) -> Option<String> {
    None
}

/// The page's `og:title`, else its `<title>`, decoded and on one line.
fn html_title(html: &str) -> Option<String> {
    let og = Regex::new(
        r#"(?is)<meta\s[^>]*?(?:property|name)\s*=\s*["']og:title["'][^>]*?content\s*=\s*["']([^"']*)["']|<meta\s[^>]*?content\s*=\s*["']([^"']*)["'][^>]*?(?:property|name)\s*=\s*["']og:title["']"#,
    )
    .expect("valid og:title regex");
    let title = Regex::new(r"(?is)<title[^>]*>(.*?)</title>").expect("valid title regex");
    let raw = og
        .captures(html)
        .and_then(|captures| captures.get(1).or_else(|| captures.get(2)))
        .or_else(|| title.captures(html).and_then(|captures| captures.get(1)))?
        .as_str();
    let decoded = decode_entities(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let clipped = decoded.chars().take(MAX_TITLE_CHARS).collect::<String>();
    (!clipped.is_empty()).then_some(clipped)
}

fn decode_entities(text: &str) -> String {
    let entity =
        Regex::new(r"&(#[0-9]+|#[xX][0-9a-fA-F]+|[a-zA-Z]+);").expect("valid entity regex");
    entity
        .replace_all(text, |captures: &regex::Captures<'_>| {
            let name = &captures[1];
            let decoded = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ if name.starts_with("#x") || name.starts_with("#X") => {
                    u32::from_str_radix(&name[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            decoded.map_or_else(|| captures[0].to_string(), String::from)
        })
        .into_owned()
}

/// Link text with brackets escaped so the Markdown link stays intact.
fn escape_link_text(title: &str) -> String {
    title
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_prefer_open_graph_and_are_decoded() {
        assert_eq!(
            html_title("<html><head><title>  Plain\n  title &amp; more </title></head>"),
            Some("Plain title & more".to_string())
        );
        assert_eq!(
            html_title(
                r#"<title>Site</title><meta property="og:title" content="Article &#8211; Site">"#
            ),
            Some("Article – Site".to_string())
        );
        assert_eq!(
            html_title(r#"<meta content="Reversed" name="og:title"><title>x</title>"#),
            Some("Reversed".to_string())
        );
        assert_eq!(html_title("<p>no title</p>"), None);
        assert_eq!(html_title("<title>   </title>"), None);
        assert_eq!(escape_link_text("[RFC] a\\b"), "\\[RFC\\] a\\\\b");
    }
}
