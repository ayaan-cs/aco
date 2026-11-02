//! A proposer backed by a local OpenAI-compatible chat server.
//!
//! Works with Ollama and with llama.cpp's `llama-server`: both serve
//! `POST {endpoint}/v1/chat/completions`. The prompt sends the whole file and
//! asks for a full replacement. Replies are parsed and pre-filtered by the
//! policy; anything that fails is dropped here and never reaches a runner.

use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

use super::{Candidate, Proposer};
use crate::policy;

pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434";
pub const DEFAULT_MODEL: &str = "qwen2.5-coder:1.5b";

const SYSTEM: &str = "You make Python programs faster. Reply with exactly one complete \
replacement file in a single ```python code block and nothing else. The new program must \
print exactly the same stdout. Use only builtins and the standard library. Do not import \
os, sys, io, subprocess, socket or any file, process or network module, and do not use \
eval, exec, compile, open, getattr or dunder attributes.";

pub struct HttpProposer {
    pub endpoint: String,
    pub model: String,
    /// Response cap sent as `max_tokens`, and the denominator of the token ratio.
    pub token_budget: u32,
    /// Requests per generation.
    pub samples: usize,
    pub timeout: Duration,
    notes: Mutex<Vec<String>>,
}

impl HttpProposer {
    pub fn new(endpoint: &str, model: &str, token_budget: u32, samples: usize) -> Self {
        HttpProposer {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            model: model.to_string(),
            token_budget,
            samples: samples.max(1),
            timeout: Duration::from_secs(120),
            notes: Mutex::new(Vec::new()),
        }
    }

    fn note(&self, msg: String) {
        if let Ok(mut n) = self.notes.lock() {
            n.push(msg);
        }
    }

    /// One chat completion. Returns the reply text and the proposal tokens.
    fn request(&self, source: &str, sample: usize) -> Result<(String, u32), String> {
        let url = format!("{}/v1/chat/completions", self.endpoint);
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": format!("Make this program faster:\n\n```python\n{source}```")},
            ],
            "max_tokens": self.token_budget,
            "temperature": 0.2 + 0.3 * sample as f64,
            "seed": sample,
            "stream": false,
        });
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .build()
            .into();
        let mut resp = agent
            .post(&url)
            .send_json(&body)
            .map_err(|e| e.to_string())?;
        let v: Value = resp.body_mut().read_json().map_err(|e| e.to_string())?;
        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .ok_or("reply has no choices[0].message.content")?
            .to_string();
        let tokens = v["usage"]["completion_tokens"]
            .as_u64()
            .map(|t| t.min(u32::MAX as u64) as u32)
            .unwrap_or_else(|| approx_tokens(&content));
        Ok((content, tokens))
    }
}

/// Token estimate when the server omits `usage`: about four bytes per token.
pub fn approx_tokens(text: &str) -> u32 {
    (text.len() as u32).div_ceil(4)
}

/// The code inside the first fenced block, or the whole reply if unfenced.
pub fn extract_code(reply: &str) -> String {
    let code = match reply.find("```") {
        Some(start) => {
            let after = &reply[start + 3..];
            let body = after.split_once('\n').map(|(_, b)| b).unwrap_or("");
            body.split("```").next().unwrap_or("")
        }
        None => reply,
    };
    let mut code = code.trim_matches('\n').to_string();
    code.push('\n');
    code
}

impl Proposer for HttpProposer {
    fn name(&self) -> String {
        format!("model:{}", self.model)
    }

    fn propose(&self, source: &str) -> Vec<Candidate> {
        let mut out = Vec::new();
        for i in 0..self.samples {
            match self.request(source, i) {
                Ok((reply, tokens)) => {
                    let code = extract_code(&reply);
                    match policy::check(&code) {
                        Ok(()) if code.trim() == source.trim() => {
                            self.note(format!("model reply {i}: identical to input, dropped"));
                        }
                        Ok(()) => out.push(Candidate {
                            source: code,
                            origin: format!("model:{}#{i}", self.model),
                            tokens,
                        }),
                        Err(v) => self.note(format!("model reply {i} dropped by policy: {v}")),
                    }
                }
                Err(e) => {
                    self.note(format!("model request to {} failed: {e}", self.endpoint));
                    break;
                }
            }
        }
        out
    }

    fn take_notes(&self) -> Vec<String> {
        self.notes
            .lock()
            .map(|mut n| std::mem::take(&mut *n))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_code() {
        assert_eq!(
            extract_code("Sure:\n```python\nprint(1)\n```\nDone."),
            "print(1)\n"
        );
        assert_eq!(extract_code("print(2)"), "print(2)\n");
    }

    #[test]
    fn approximates_tokens_from_bytes() {
        assert_eq!(approx_tokens("abcd"), 1);
        assert_eq!(approx_tokens("abcde"), 2);
    }
}
