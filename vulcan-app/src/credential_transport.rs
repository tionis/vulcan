use crate::AppError;
use reqwest::Url;
use std::net::IpAddr;

pub(crate) fn validate_credential_transport(
    endpoint: &Url,
    credential_present: bool,
    label: &str,
) -> Result<(), AppError> {
    if !credential_present || endpoint.scheme() == "https" || is_loopback_http(endpoint) {
        return Ok(());
    }

    Err(AppError::operation(format!(
        "{label} credentials require HTTPS unless the endpoint uses loopback HTTP"
    )))
}

fn is_loopback_http(endpoint: &Url) -> bool {
    endpoint.scheme() == "http"
        && endpoint.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_require_https_or_a_loopback_http_endpoint() {
        for allowed in [
            "https://api.example.com/v1",
            "http://localhost:11434/v1",
            "http://127.0.0.2:11434/v1",
            "http://[::1]:11434/v1",
        ] {
            validate_credential_transport(&Url::parse(allowed).unwrap(), true, "agent")
                .expect("secure or loopback endpoint");
        }

        let error = validate_credential_transport(
            &Url::parse("http://api.example.com/v1").unwrap(),
            true,
            "agent",
        )
        .expect_err("remote cleartext endpoint must fail");
        assert!(error.to_string().contains("credentials require HTTPS"));

        validate_credential_transport(
            &Url::parse("http://api.example.com/v1").unwrap(),
            false,
            "agent",
        )
        .expect("credential-free HTTP remains available");
    }
}
