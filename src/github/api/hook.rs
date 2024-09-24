use axum::extract::{FromRequest, Request};
use hmac::Mac;
use reqwest::StatusCode;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::github::AppState;

use super::{
    check::{CheckRun, CheckRunAction, CheckSuite, CheckSuiteAction},
    Installation, Repository,
};

pub enum HookPayload {
    Installation(InstallationPayload),
    CheckSuite(CheckSuitePayload),
    CheckRun(CheckRunPayload),
}

impl HookPayload {
    pub fn installation(&self) -> &Installation {
        match self {
            HookPayload::CheckSuite(cs) => &cs.installation,
            HookPayload::Installation(i) => &i.installation,
            HookPayload::CheckRun(cr) => &cr.installation,
        }
    }
}

/// Request extractor that reads and check a GitHub hook payload.
#[async_trait::async_trait]
impl FromRequest<AppState> for HookPayload {
    type Rejection = (StatusCode, &'static str);

    async fn from_request<'s>(req: Request, state: &'s AppState) -> Result<Self, Self::Rejection> {
        debug!("Received a webhook event…");
        let event_type = req
            .headers()
            .get("X-GitHub-Event")
            .map(|v| v.as_bytes().to_owned());
        debug!("Event type is {:?}", event_type);

        let Some(their_signature_header) = req.headers().get("X-Hub-Signature") else {
            return Err((StatusCode::UNAUTHORIZED, "X-Hub-Signature is missing"));
        };
        let their_signature_header = their_signature_header
            .to_str()
            .unwrap_or_default()
            .to_owned();

        let Some((method, their_digest)) = their_signature_header.split_once('=') else {
            return Err((StatusCode::BAD_REQUEST, "Malformed signature header"));
        };

        if method != "sha1" {
            warn!(
                "A hook with a {} signature was received, and rejected",
                method
            );
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "Unsupported signature type",
            ));
        }

        let Ok(raw_payload) = String::from_request(req, state).await else {
            return Err((StatusCode::BAD_REQUEST, "Cannot read request body."));
        };

        let our_digest = {
            let Ok(mut mac) = hmac::Hmac::<sha1::Sha1>::new_from_slice(&state.webhook_secret)
            else {
                warn!("Webhook secret is invalid.");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Server is not correctly configured.",
                ));
            };
            mac.update(raw_payload.as_bytes());
            mac
        };
        // GitHub provides their hash as a hexadecimal string.
        let parsed_digest: Vec<_> = (0..their_digest.len() / 2)
            .filter_map(|idx| {
                let slice = &their_digest[idx * 2..idx * 2 + 2];
                u8::from_str_radix(slice, 16).ok()
            })
            .collect();
        if our_digest.verify_slice(&parsed_digest).is_err() {
            debug!("Invalid hook signature");
            return Err((StatusCode::UNAUTHORIZED, "Invalid hook signature"));
        }

        macro_rules! try_deser {
            ($variant:ident, $json:expr) => {
                match serde_json::from_str($json) {
                    Ok(x) => Ok(HookPayload::$variant(x)),
                    Err(_) => return Err((StatusCode::BAD_REQUEST, "Invalid JSON data")),
                }
            };
        }

        match event_type.as_deref() {
            Some(b"installation") => try_deser!(Installation, &raw_payload),
            Some(b"check_suite") => try_deser!(CheckSuite, &raw_payload),
            Some(b"check_run") => try_deser!(CheckRun, &raw_payload),
            Some(x) => {
                debug!(
                    "Uknown event type: {}",
                    std::str::from_utf8(x).unwrap_or("[UTF-8 error]")
                );
                debug!("Payload was: {}", raw_payload);
                Err((StatusCode::BAD_REQUEST, "Unknown event type"))
            }
            None => Err((StatusCode::BAD_REQUEST, "Unspecified event type")),
        }
    }
}

#[derive(Deserialize)]
pub struct InstallationPayload {
    installation: Installation,
}

#[derive(Deserialize)]
pub struct CheckSuitePayload {
    pub action: CheckSuiteAction,
    pub installation: Installation,
    pub repository: Repository,
    pub check_suite: CheckSuite,
}

#[derive(Deserialize)]
pub struct CheckRunPayload {
    pub installation: Installation,
    pub action: CheckRunAction,
    pub repository: Repository,
    pub check_run: CheckRun,
}
