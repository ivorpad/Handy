use crate::settings::{
    get_settings, PostProcessApiFormat, PostProcessProvider, APPLE_INTELLIGENCE_PROVIDER_ID,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use tauri::AppHandle;

/// Generate text with an on-device provider. Callers retain their
/// input and decide whether the result should be saved or inserted.
pub async fn generate(app: &AppHandle, instruction: &str, input: &str) -> Result<String, String> {
    if instruction.trim().is_empty() || input.trim().is_empty() {
        return Err("Provide an instruction and some text first.".into());
    }
    if instruction.chars().count() > 16_000 || input.chars().count() > 200_000 {
        return Err("This request is too large. Select a smaller portion of text.".into());
    }
    let settings = get_settings(app);
    let provider = settings
        .active_post_process_provider()
        .cloned()
        .ok_or("Choose an AI provider in Post Processing settings first.")?;
    let model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    if model.trim().is_empty() {
        return Err("Choose a model in Post Processing settings first.".into());
    }
    if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let instruction = instruction.to_owned();
            let input = input.to_owned();
            let token_limit = model.trim().parse::<i32>().unwrap_or(0);
            return tauri::async_runtime::spawn_blocking(move || {
                if !crate::apple_intelligence::check_apple_intelligence_availability() {
                    return Err("Apple Intelligence is unavailable on this device.".to_string());
                }
                crate::apple_intelligence::process_text_with_system_prompt(
                    &instruction,
                    &input,
                    token_limit,
                )
                .map_err(|error| error.to_string())
                .and_then(nonempty)
            })
            .await
            .map_err(|_| "The local generation worker stopped unexpectedly.".to_string())?;
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err("Apple Intelligence is unavailable on this platform.".into());
    }

    let api_key = settings
        .post_process_api_keys
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    local_completion(&provider, &api_key, &model, instruction, input).await
}

fn local_endpoint(base: &str, format: &PostProcessApiFormat) -> Result<reqwest::Url, String> {
    let mut url = reqwest::Url::parse(base).map_err(|_| "Choose a valid local model URL.")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Local model URLs must use HTTP(S) without credentials, a query or fragment.".into(),
        );
    }
    let host = url.host_str().unwrap_or("");
    let loopback = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        return Err("Workspace generation stays on this device. Choose Apple Intelligence or a model server at localhost, 127.0.0.1 or ::1 in Post Processing settings.".into());
    }
    let suffix = if *format == PostProcessApiFormat::Anthropic {
        "messages"
    } else {
        "chat/completions"
    };
    url.set_path(&format!("{}/{suffix}", url.path().trim_end_matches('/')));
    Ok(url)
}

pub(crate) fn local_client_builder(base: &str) -> Result<reqwest::ClientBuilder, String> {
    local_endpoint(base, &PostProcessApiFormat::OpenAi)?;
    // Pin localhost rather than consulting DNS/hosts. A loopback server cannot
    // redirect a transcript off-device, and environment proxies are excluded.
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .resolve(
            "localhost",
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        )
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(180)))
}

async fn local_completion(
    provider: &PostProcessProvider,
    api_key: &str,
    model: &str,
    instruction: &str,
    input: &str,
) -> Result<String, String> {
    let endpoint = local_endpoint(&provider.base_url, &provider.api_format)?;
    let client = local_client_builder(&provider.base_url)?
        .user_agent("Handy-Local/1.0")
        .build()
        .map_err(|_| "Could not initialize the local model client.")?;
    let anthropic = provider.api_format == PostProcessApiFormat::Anthropic;
    let body = if anthropic {
        serde_json::json!({"model":model,"max_tokens":4096,"system":instruction,"messages":[{"role":"user","content":input}],"stream":false})
    } else {
        serde_json::json!({"model":model,"messages":[{"role":"system","content":instruction},{"role":"user","content":input}],"stream":false})
    };
    let mut request = client.post(endpoint).json(&body);
    if anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }
    if !api_key.is_empty() {
        let value = if anthropic {
            api_key.to_owned()
        } else {
            format!("Bearer {api_key}")
        };
        let value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| "The local model API key contains invalid header characters.")?;
        request = request.header(
            if anthropic {
                "x-api-key"
            } else {
                "authorization"
            },
            value,
        );
    }
    let mut response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            "The local model timed out. Your text is preserved."
        } else {
            "Cannot reach the local model. Start its server and check Post Processing settings."
        }
    })?;
    if !response.status().is_success() {
        return Err(format!("The local model returned HTTP {}. Redirects are not followed and your text is preserved.", response.status().as_u16()));
    }
    const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "The local model response was interrupted.")?
    {
        if bytes.len().saturating_add(chunk.len()) > RESPONSE_LIMIT {
            return Err("The local model response exceeds the size limit.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "The local model returned invalid JSON.")?;
    let text = if anthropic {
        if value["stop_reason"] == "max_tokens" {
            return Err("The local model response was truncated.".into());
        }
        value["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("")
    } else {
        let choice = &value["choices"][0];
        if choice["finish_reason"] == "length" {
            return Err("The local model response was truncated.".into());
        }
        choice["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_owned()
    };
    nonempty(text)
}

fn nonempty(text: String) -> Result<String, String> {
    let trimmed = text.trim();
    let output = if trimmed.starts_with("<think>") {
        trimmed
            .split_once("</think>")
            .map(|(_, rest)| rest.trim())
            .unwrap_or("")
    } else {
        trimmed
    };
    if output.is_empty() {
        Err("The model returned no text. Your original text is unchanged.".into())
    } else {
        Ok(output.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_rejects_remote_and_credential_bearing_urls() {
        for base in [
            "https://api.openai.com/v1",
            "http://localhost.example/v1",
            "http://192.168.1.2/v1",
            "http://localtest.me/v1",
            "http://secret@127.0.0.1/v1",
            "http://127.0.0.1/v1?key=secret",
            "file:///tmp/model",
        ] {
            assert!(
                local_endpoint(base, &PostProcessApiFormat::OpenAi).is_err(),
                "{base}"
            );
        }
        for base in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080/v1",
            "http://[::1]:8080/v1",
        ] {
            assert!(local_endpoint(base, &PostProcessApiFormat::OpenAi)
                .unwrap()
                .path()
                .ends_with("/chat/completions"));
        }
    }

    #[tokio::test]
    async fn loopback_redirect_cannot_forward_source_text() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut provider = crate::settings::get_default_settings()
            .post_process_providers
            .into_iter()
            .find(|p| p.id == "custom")
            .unwrap();
        provider.base_url = format!("http://{}/v1", server.local_addr().unwrap());
        let destination = target.local_addr().unwrap();
        let serving = tokio::spawn(async move {
            let (mut socket, _) = server.accept().await.unwrap();
            let mut buffer = [0; 4096];
            let received = socket.read(&mut buffer).await.unwrap();
            assert!(received > 0, "Expected a request to the loopback server");
            socket.write_all(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{destination}/leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            local_completion(
                &provider,
                "",
                "local",
                "Summarize",
                "synthetic private text",
            ),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().contains("307"));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), target.accept())
                .await
                .is_err()
        );
        serving.await.unwrap();
    }
}
