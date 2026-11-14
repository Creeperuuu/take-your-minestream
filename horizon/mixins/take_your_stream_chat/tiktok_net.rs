// HTTPS and gzip for the TikTok reader, with the `ureq` and `flate2` crates that
// horizon-client already depends on. The bundle adds this file to the game next to
// runtime.rs (as `game::take_your_stream_chat_net`); runtime.rs's tests use a stub.

use std::io::Read;
use std::time::Duration;

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
/// The most one response or one gunzipped payload may be.
const MAX_BYTES: u64 = 16 * 1024 * 1024;

/// A response's body and its `Set-Cookie` headers.
pub struct Reply {
    pub body: Vec<u8>,
    pub set_cookies: Vec<String>,
}

/// A GET, or a POST of `post_json`, with the extra `headers`; any non-2xx answer is an error.
pub fn request(
    post_json: Option<&str>,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<Reply, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .redirects(5)
        .build();
    let mut req = match post_json {
        Some(_) => agent.post(url),
        None => agent.get(url),
    }
    .set("User-Agent", USER_AGENT)
    .set("Accept-Language", "en-US,en;q=0.9");
    for (name, value) in headers {
        req = req.set(name, value);
    }
    let result = match post_json {
        Some(body) => req
            .set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_string(body),
        None => req.call(),
    };
    let response = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("TikTok answered {code}")),
        Err(ureq::Error::Transport(t)) => return Err(format!("TikTok can't be reached: {t}")),
    };
    let set_cookies = response
        .all("set-cookie")
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_BYTES)
        .read_to_end(&mut body)
        .map_err(|e| format!("TikTok's answer broke off: {e}"))?;
    Ok(Reply { body, set_cookies })
}

/// A gzip payload, unpacked.
pub fn gunzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(MAX_BYTES)
        .read_to_end(&mut out)
        .map_err(|e| format!("bad gzip from TikTok: {e}"))?;
    Ok(out)
}
