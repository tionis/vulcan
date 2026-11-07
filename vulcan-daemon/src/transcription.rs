//! Speech-to-text for voice messages through an OpenAI-compatible
//! `/audio/transcriptions` endpoint.

use crate::registry::DaemonTranscriptionConfig;
use std::time::Duration;

const TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(120);
/// The most of a transcript kept; longer ones are cut at a word.
const MAX_TRANSCRIPT_CHARS: usize = 4000;
const MAX_ERROR_CHARS: usize = 200;

/// The text spoken in `bytes`, on one line. Errors never contain the URL or
/// the API key.
pub fn transcribe(
    config: &DaemonTranscriptionConfig,
    file_name: &str,
    bytes: Vec<u8>,
) -> Result<String, String> {
    let api_key = match &config.api_key_env {
        Some(name) => Some(
            std::env::var(name)
                .ok()
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
                .ok_or_else(|| format!("{name} is not set"))?,
        ),
        None => None,
    };
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(TRANSCRIPTION_TIMEOUT)
        .build()
        .map_err(|error| error.without_url().to_string())?;
    let part = reqwest::blocking::multipart::Part::bytes(bytes)
        .file_name(file_name.to_string())
        .mime_str(mime_for(file_name))
        .map_err(|error| error.without_url().to_string())?;
    let mut form = reqwest::blocking::multipart::Form::new()
        .part("file", part)
        .text("model", config.model.clone())
        .text("response_format", "json");
    if let Some(language) = &config.language {
        form = form.text("language", language.clone());
    }
    let mut request = client
        .post(format!(
            "{}/audio/transcriptions",
            config.base_url.trim_end_matches('/')
        ))
        .multipart(form);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .map_err(|error| error.without_url().to_string())?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .map_err(|_| format!("the service answered {status} without JSON"))?;
    if !status.is_success() {
        let message = body
            .pointer("/error/message")
            .or_else(|| body.get("error"))
            .or_else(|| body.get("detail"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("no details");
        return Err(format!(
            "the service answered {status}: {}",
            clip(message, MAX_ERROR_CHARS)
        ));
    }
    let text = body
        .get("text")
        .and_then(serde_json::Value::as_str)
        .ok_or("the service returned no text")?;
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("nothing was recognized".to_string());
    }
    Ok(clip(&text, MAX_TRANSCRIPT_CHARS))
}

fn mime_for(file_name: &str) -> &'static str {
    match file_name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("webm") => "audio/webm",
        _ => "audio/ogg",
    }
}

/// `text` cut to `limit` characters at a word, with an ellipsis.
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut = text.chars().take(limit).collect::<String>();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_clipped_at_a_word() {
        assert_eq!(clip("short", 10), "short");
        assert_eq!(clip("one two three", 9), "one two…");
        assert_eq!(mime_for("voice-1.ogg"), "audio/ogg");
        assert_eq!(mime_for("song.mp3"), "audio/mpeg");
    }
}
