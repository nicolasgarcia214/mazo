use std::path::Path;

use serde_json::{Value, json};

use super::{JUDGE_TIMEOUT, NoAnswer, Reply, STDERR_CHARS, prompted};
use crate::audit::Stage;
use crate::config::{Billing, Judge};
use crate::{Error, Result};

pub struct Http {
    endpoint: String,
    model: String,
    key: Option<(String, String)>,
}

impl Http {
    pub fn new(judge: &Judge) -> Result<Http> {
        let (url, model) = judge.url.as_ref().zip(judge.model.as_ref()).expect("an http judge has a url and a model");
        let key = match &judge.api_key_env {
            None => None,
            Some(var) => match std::env::var(var) {
                Ok(key) if !key.is_empty() => Some((var.clone(), key)),
                _ => return Err(Error::Usage(format!("[judge] api_key_env names {var}, which is unset or empty"))),
            },
        };
        let endpoint = format!("{}/chat/completions", url.trim_end_matches('/'));
        Ok(Http { endpoint, model: model.clone(), key })
    }
}

impl super::Backend for Http {
    fn call(
        &self,
        _data: &Path,
        _billing: Billing,
        stage: &Stage,
        _cap: Option<u64>,
        input: &str,
        _cancel: &dyn Fn() -> bool,
    ) -> Result<Reply> {
        let body = json!({"model": self.model, "messages": [{"role": "user", "content": prompted(stage, input)}]});
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(JUDGE_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = agent.post(&self.endpoint);
        if let Some((_, key)) = &self.key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let sent = request.send_json(&body).and_then(|mut r| Ok((r.status(), r.body_mut().read_to_string()?)));
        let answer = match sent {
            Err(ureq::Error::Timeout(_)) => {
                Err(NoAnswer::Retry(format!("no answer within {} minutes", JUDGE_TIMEOUT.as_secs() / 60)))
            }
            Err(e) => Err(NoAnswer::Stop(format!("{}: {e}", self.endpoint))),
            Ok((status, text)) if !status.is_success() => {
                let mut why: String = text.lines().next().unwrap_or_default().chars().take(STDERR_CHARS).collect();
                if let Some((_, key)) = &self.key {
                    why = why.replace(key.as_str(), "<key>");
                }
                Err(NoAnswer::Stop(format!("{} returned {status}: {why}", self.endpoint)))
            }
            Ok((_, text)) => content(&text).map_err(NoAnswer::Retry),
        };
        Ok(Reply { cost: None, answer })
    }

    fn dry_run(&self) -> String {
        let auth = match &self.key {
            Some((var, _)) => format!("a Bearer token from ${var}"),
            None => "no Authorization header".into(),
        };
        format!("POST {} with {auth}", self.endpoint)
    }

    fn model(&self) -> Option<String> {
        Some(self.model.clone())
    }
}

fn content(reply: &str) -> std::result::Result<Value, String> {
    let reply: Value = serde_json::from_str(reply).map_err(|e| format!("the reply is not JSON: {e}"))?;
    let content = reply
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or("the reply has no choices[0].message.content")?;
    serde_json::from_str(content).map_err(|e| format!("the answer is not JSON: {e}"))
}
