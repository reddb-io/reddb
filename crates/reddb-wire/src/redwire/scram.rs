//! SCRAM-SHA-256 primitives and sans-I/O client/server handshake drivers.
//!
//! The drivers own SCRAM message sequencing and proof validation. Transport I/O,
//! credential lookup, session creation, and authorization policy remain adapter concerns.

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use super::handshake::{
    base64_std, base64_std_decode, build_scram_server_first, expect_auth_response_payload,
    parse_scram_client_final, parse_scram_client_first, AuthFail, AuthOk,
};
use super::MessageKind;

/// Default iteration count for newly-created RedDB SCRAM verifiers.
pub const DEFAULT_ITER: u32 = 16_384;

/// Minimum accepted iteration count for persisted SCRAM verifiers.
pub const MIN_ITER: u32 = 4096;

/// Persisted SCRAM verifier. It never contains the plaintext password or salted password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScramVerifier {
    pub salt: Vec<u8>,
    pub iter: u32,
    pub stored_key: [u8; 32],
    pub server_key: [u8; 32],
}

impl ScramVerifier {
    pub fn from_password(password: &str, salt: Vec<u8>, iter: u32) -> Self {
        let salted = salted_password(password.as_bytes(), &salt, iter);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        let server_key = hmac_sha256(&salted, b"Server Key");
        Self {
            salt,
            iter,
            stored_key,
            server_key,
        }
    }
}

/// One adapter action produced by a [`ScramClientHandshake`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScramClientOutput {
    Response(Vec<u8>),
    Authenticated(AuthOk),
}

/// Terminal client-side SCRAM failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScramClientError {
    Refused(String),
    Protocol(String),
}

impl std::fmt::Display for ScramClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => write!(formatter, "SCRAM authentication refused: {reason}"),
            Self::Protocol(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for ScramClientError {}

/// Transport-independent SCRAM-SHA-256 client exchange.
pub struct ScramClientHandshake {
    state: ScramClientState,
}

enum ScramClientState {
    Ready {
        username: String,
        password: String,
        client_nonce: String,
    },
    AwaitServerFirst {
        password: String,
        client_nonce: String,
        client_first_bare: String,
    },
    AwaitServerFinal {
        expected_server_signature: [u8; 32],
    },
    Complete,
}

impl ScramClientHandshake {
    /// Create a client driver with adapter-supplied randomness.
    pub fn new(
        username: impl Into<String>,
        password: impl Into<String>,
        client_nonce: impl Into<String>,
    ) -> Result<Self, ScramClientError> {
        let username = username.into();
        if username.is_empty() || username.contains([',', '=']) {
            return Err(ScramClientError::Protocol(
                "SCRAM username must be non-empty and contain neither ',' nor '='".to_string(),
            ));
        }
        let client_nonce = client_nonce.into();
        if client_nonce.is_empty() || client_nonce.contains(',') {
            return Err(ScramClientError::Protocol(
                "SCRAM client nonce must be non-empty and contain no ','".to_string(),
            ));
        }
        Ok(Self {
            state: ScramClientState::Ready {
                username,
                password: password.into(),
                client_nonce,
            },
        })
    }

    /// Start the exchange by producing the client-first-message.
    pub fn start(&mut self) -> Result<ScramClientOutput, ScramClientError> {
        let state = std::mem::replace(&mut self.state, ScramClientState::Complete);
        let ScramClientState::Ready {
            username,
            password,
            client_nonce,
        } = state
        else {
            return Err(unexpected_client_state());
        };
        let client_first_bare = format!("n={username},r={client_nonce}");
        let response = format!("n,,{client_first_bare}").into_bytes();
        self.state = ScramClientState::AwaitServerFirst {
            password,
            client_nonce,
            client_first_bare,
        };
        Ok(ScramClientOutput::Response(response))
    }

    /// Consume one server frame and produce the next adapter action.
    pub fn step(
        &mut self,
        kind: MessageKind,
        payload: &[u8],
    ) -> Result<ScramClientOutput, ScramClientError> {
        let state = std::mem::replace(&mut self.state, ScramClientState::Complete);
        match state {
            ScramClientState::AwaitServerFirst {
                password,
                client_nonce,
                client_first_bare,
            } => {
                self.receive_server_first(kind, payload, password, client_nonce, client_first_bare)
            }
            ScramClientState::AwaitServerFinal {
                expected_server_signature,
            } => Self::receive_server_final(kind, payload, &expected_server_signature),
            ScramClientState::Ready { .. } | ScramClientState::Complete => {
                Err(unexpected_client_state())
            }
        }
    }

    fn receive_server_first(
        &mut self,
        kind: MessageKind,
        payload: &[u8],
        password: String,
        client_nonce: String,
        client_first_bare: String,
    ) -> Result<ScramClientOutput, ScramClientError> {
        if kind == MessageKind::AuthFail {
            return Err(auth_refused(payload));
        }
        if kind != MessageKind::AuthRequest {
            return Err(ScramClientError::Protocol(format!(
                "expected AuthRequest(server-first-message), got {kind:?}"
            )));
        }
        let server_first = std::str::from_utf8(payload).map_err(|_| {
            ScramClientError::Protocol("SCRAM server-first is not UTF-8".to_string())
        })?;
        let (combined_nonce, salt, iter) = parse_server_first(server_first, &client_nonce)?;
        let client_final_no_proof = format!("c=biws,r={combined_nonce}");
        let message = auth_message(&client_first_bare, server_first, &client_final_no_proof);
        let salted = salted_password(password.as_bytes(), &salt, iter);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        let proof = client_proof(&stored_key, &message, &client_key);
        let expected_server_signature =
            server_signature(&hmac_sha256(&salted, b"Server Key"), &message);
        let response = format!("{client_final_no_proof},p={}", base64_std(&proof)).into_bytes();
        self.state = ScramClientState::AwaitServerFinal {
            expected_server_signature,
        };
        Ok(ScramClientOutput::Response(response))
    }

    fn receive_server_final(
        kind: MessageKind,
        payload: &[u8],
        expected_server_signature: &[u8; 32],
    ) -> Result<ScramClientOutput, ScramClientError> {
        if kind == MessageKind::AuthFail {
            return Err(auth_refused(payload));
        }
        if kind != MessageKind::AuthOk {
            return Err(ScramClientError::Protocol(format!(
                "expected AuthOk/AuthFail, got {kind:?}"
            )));
        }
        let auth_ok = AuthOk::from_payload(payload)
            .map_err(|error| ScramClientError::Protocol(format!("decode SCRAM AuthOk: {error}")))?;
        let encoded_signature = auth_ok.server_signature.as_deref().ok_or_else(|| {
            ScramClientError::Protocol("SCRAM AuthOk omitted server signature".to_string())
        })?;
        let presented_signature = base64_std_decode(encoded_signature).ok_or_else(|| {
            ScramClientError::Protocol(
                "SCRAM AuthOk server signature is not valid base64".to_string(),
            )
        })?;
        if !constant_time_eq(expected_server_signature, &presented_signature) {
            return Err(ScramClientError::Protocol(
                "SCRAM server signature did not verify".to_string(),
            ));
        }
        Ok(ScramClientOutput::Authenticated(auth_ok))
    }
}

fn parse_server_first(
    server_first: &str,
    client_nonce: &str,
) -> Result<(String, Vec<u8>, u32), ScramClientError> {
    let mut combined_nonce = None;
    let mut encoded_salt = None;
    let mut iterations = None;
    for field in server_first.split(',') {
        if let Some(value) = field.strip_prefix("r=") {
            set_once(&mut combined_nonce, value, "r")?;
        } else if let Some(value) = field.strip_prefix("s=") {
            set_once(&mut encoded_salt, value, "s")?;
        } else if let Some(value) = field.strip_prefix("i=") {
            set_once(&mut iterations, value, "i")?;
        }
    }

    let combined_nonce = combined_nonce.ok_or_else(|| {
        ScramClientError::Protocol("SCRAM server-first omitted r=<nonce>".to_string())
    })?;
    if !combined_nonce.starts_with(client_nonce) || combined_nonce == client_nonce {
        return Err(ScramClientError::Protocol(
            "server nonce does not extend client nonce".to_string(),
        ));
    }
    let encoded_salt = encoded_salt.ok_or_else(|| {
        ScramClientError::Protocol("SCRAM server-first omitted s=<salt>".to_string())
    })?;
    let salt = base64_std_decode(encoded_salt)
        .ok_or_else(|| ScramClientError::Protocol("server salt is not valid base64".to_string()))?;
    if salt.is_empty() {
        return Err(ScramClientError::Protocol(
            "server salt is empty".to_string(),
        ));
    }
    let iterations = iterations
        .ok_or_else(|| {
            ScramClientError::Protocol("SCRAM server-first omitted i=<iterations>".to_string())
        })?
        .parse::<u32>()
        .map_err(|_| {
            ScramClientError::Protocol("SCRAM iteration count is not an integer".to_string())
        })?;
    if iterations < MIN_ITER {
        return Err(ScramClientError::Protocol(format!(
            "SCRAM iteration count {iterations} is below minimum {MIN_ITER}"
        )));
    }
    Ok((combined_nonce.to_string(), salt, iterations))
}

fn set_once<'a>(
    destination: &mut Option<&'a str>,
    value: &'a str,
    name: &str,
) -> Result<(), ScramClientError> {
    if destination.replace(value).is_some() {
        return Err(ScramClientError::Protocol(format!(
            "SCRAM server-first repeated {name}= field"
        )));
    }
    Ok(())
}

fn auth_refused(payload: &[u8]) -> ScramClientError {
    match AuthFail::from_payload(payload) {
        Ok(failure) => ScramClientError::Refused(failure.reason),
        Err(error) => ScramClientError::Protocol(format!("decode SCRAM AuthFail: {error}")),
    }
}

fn unexpected_client_state() -> ScramClientError {
    ScramClientError::Protocol("unexpected SCRAM client message".to_string())
}

/// One external event consumed by [`ScramServerHandshake::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScramServerInput {
    ClientMessage {
        correlation_id: u64,
        kind: MessageKind,
        payload: Vec<u8>,
    },
    Verifier(Option<ScramVerifier>),
}

/// One adapter action produced by [`ScramServerHandshake::step`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScramServerOutput {
    NeedVerifier {
        username: String,
    },
    Challenge {
        correlation_id: u64,
        payload: Vec<u8>,
    },
    Authenticated {
        correlation_id: u64,
        username: String,
        server_signature: [u8; 32],
    },
    Failed {
        correlation_id: u64,
        reason: String,
    },
}

/// Transport-independent SCRAM-SHA-256 server exchange.
#[derive(Debug)]
pub struct ScramServerHandshake {
    server_nonce: String,
    unknown_user_salt: Vec<u8>,
    state: ScramServerState,
}

#[derive(Debug)]
enum ScramServerState {
    AwaitClientFirst,
    AwaitVerifier {
        correlation_id: u64,
        username: String,
        client_nonce: String,
        client_first_bare: String,
    },
    AwaitClientFinal(ScramClientFinalState),
    Complete,
}

#[derive(Debug)]
struct ScramClientFinalState {
    username: String,
    combined_nonce: String,
    client_first_bare: String,
    server_first: String,
    verifier: ScramVerifier,
    user_known: bool,
}

impl ScramServerHandshake {
    /// Create a driver with adapter-supplied randomness.
    ///
    /// `unknown_user_salt` is sent with a dummy verifier so unknown users follow the same
    /// challenge/response shape as known users.
    pub fn new(server_nonce: impl Into<String>, unknown_user_salt: Vec<u8>) -> Self {
        Self {
            server_nonce: server_nonce.into(),
            unknown_user_salt,
            state: ScramServerState::AwaitClientFirst,
        }
    }

    pub fn step(&mut self, input: ScramServerInput) -> ScramServerOutput {
        let state = std::mem::replace(&mut self.state, ScramServerState::Complete);
        match (state, input) {
            (
                ScramServerState::AwaitClientFirst,
                ScramServerInput::ClientMessage {
                    correlation_id,
                    kind,
                    payload,
                },
            ) => self.step_receive_client_first(correlation_id, kind, &payload),
            (
                ScramServerState::AwaitVerifier {
                    correlation_id,
                    username,
                    client_nonce,
                    client_first_bare,
                },
                ScramServerInput::Verifier(verifier),
            ) => self.step_receive_verifier(
                correlation_id,
                username,
                client_nonce,
                client_first_bare,
                verifier,
            ),
            (
                ScramServerState::AwaitClientFinal(final_state),
                ScramServerInput::ClientMessage {
                    correlation_id,
                    kind,
                    payload,
                },
            ) => Self::step_receive_client_final(correlation_id, kind, &payload, final_state),
            (_, ScramServerInput::ClientMessage { correlation_id, .. }) => {
                ScramServerOutput::Failed {
                    correlation_id,
                    reason: "unexpected SCRAM client message".to_string(),
                }
            }
            (_, ScramServerInput::Verifier(_)) => ScramServerOutput::Failed {
                correlation_id: 0,
                reason: "unexpected SCRAM verifier".to_string(),
            },
        }
    }

    fn step_receive_client_first(
        &mut self,
        correlation_id: u64,
        kind: MessageKind,
        payload: &[u8],
    ) -> ScramServerOutput {
        let payload =
            match expect_auth_response_payload(kind, payload, "AuthResponse(client-first-message)")
            {
                Ok(payload) => payload,
                Err(error) => return failure(correlation_id, error.to_string()),
            };
        let (username, client_nonce, client_first_bare) = match parse_scram_client_first(payload) {
            Ok(parsed) => parsed,
            Err(error) => return failure(correlation_id, format!("scram client-first: {error}")),
        };
        self.state = ScramServerState::AwaitVerifier {
            correlation_id,
            username: username.clone(),
            client_nonce,
            client_first_bare,
        };
        ScramServerOutput::NeedVerifier { username }
    }

    fn step_receive_verifier(
        &mut self,
        correlation_id: u64,
        username: String,
        client_nonce: String,
        client_first_bare: String,
        verifier: Option<ScramVerifier>,
    ) -> ScramServerOutput {
        let user_known = verifier.is_some();
        let verifier = verifier.unwrap_or_else(|| ScramVerifier {
            salt: self.unknown_user_salt.clone(),
            iter: DEFAULT_ITER,
            stored_key: [0; 32],
            server_key: [0; 32],
        });
        let server_first = build_scram_server_first(
            &client_nonce,
            &self.server_nonce,
            &verifier.salt,
            verifier.iter,
        );
        let combined_nonce = format!("{client_nonce}{}", self.server_nonce);
        self.state = ScramServerState::AwaitClientFinal(ScramClientFinalState {
            username,
            combined_nonce,
            client_first_bare,
            server_first: server_first.clone(),
            verifier,
            user_known,
        });
        ScramServerOutput::Challenge {
            correlation_id,
            payload: server_first.into_bytes(),
        }
    }

    fn step_receive_client_final(
        correlation_id: u64,
        kind: MessageKind,
        payload: &[u8],
        state: ScramClientFinalState,
    ) -> ScramServerOutput {
        let payload =
            match expect_auth_response_payload(kind, payload, "AuthResponse(client-final-message)")
            {
                Ok(payload) => payload,
                Err(error) => return failure(correlation_id, error.to_string()),
            };
        let (combined_nonce, presented_proof, client_final_no_proof) =
            match parse_scram_client_final(payload) {
                Ok(parsed) => parsed,
                Err(error) => {
                    return failure(correlation_id, format!("scram client-final: {error}"))
                }
            };
        if combined_nonce != state.combined_nonce {
            return failure(
                correlation_id,
                "scram nonce mismatch — replay protection failed".to_string(),
            );
        }
        let auth_message = auth_message(
            &state.client_first_bare,
            &state.server_first,
            &client_final_no_proof,
        );
        if !state.user_known
            || !verify_client_proof(&state.verifier, &auth_message, &presented_proof)
        {
            return failure(correlation_id, "invalid SCRAM proof".to_string());
        }
        ScramServerOutput::Authenticated {
            correlation_id,
            username: state.username,
            server_signature: server_signature(&state.verifier.server_key, &auth_message),
        }
    }
}

fn failure(correlation_id: u64, reason: String) -> ScramServerOutput {
    ScramServerOutput::Failed {
        correlation_id,
        reason,
    }
}

/// Compute `SaltedPassword` using PBKDF2-HMAC-SHA256.
pub fn salted_password(password: &[u8], salt: &[u8], iter: u32) -> [u8; 32] {
    let mut salt_with_block = Vec::with_capacity(salt.len() + 4);
    salt_with_block.extend_from_slice(salt);
    salt_with_block.extend_from_slice(&1u32.to_be_bytes());

    let mut round = hmac_sha256(password, &salt_with_block);
    let mut result = round;
    for _ in 1..iter {
        round = hmac_sha256(password, &round);
        for (result_byte, round_byte) in result.iter_mut().zip(round) {
            *result_byte ^= round_byte;
        }
    }
    result
}

pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn xor(left: &[u8], right: &[u8]) -> Vec<u8> {
    assert_eq!(
        left.len(),
        right.len(),
        "SCRAM XOR operands must have equal lengths"
    );
    left.iter()
        .zip(right)
        .map(|(left_byte, right_byte)| left_byte ^ right_byte)
        .collect()
}

pub fn auth_message(
    client_first_bare: &str,
    server_first: &str,
    client_final_no_proof: &str,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        client_first_bare.len() + server_first.len() + client_final_no_proof.len() + 2,
    );
    message.extend_from_slice(client_first_bare.as_bytes());
    message.push(b',');
    message.extend_from_slice(server_first.as_bytes());
    message.push(b',');
    message.extend_from_slice(client_final_no_proof.as_bytes());
    message
}

pub fn client_proof(stored_key: &[u8], auth_message: &[u8], client_key: &[u8]) -> Vec<u8> {
    xor(client_key, &hmac_sha256(stored_key, auth_message))
}

pub fn verify_client_proof(
    verifier: &ScramVerifier,
    auth_message: &[u8],
    presented_proof: &[u8],
) -> bool {
    if presented_proof.len() != 32 {
        return false;
    }
    let client_signature = hmac_sha256(&verifier.stored_key, auth_message);
    let client_key = xor(presented_proof, &client_signature);
    constant_time_eq(&sha256(&client_key), &verifier.stored_key)
}

pub fn server_signature(server_key: &[u8], auth_message: &[u8]) -> [u8; 32] {
    hmac_sha256(server_key, auth_message)
}

pub fn verify_server_signature(
    server_key: &[u8],
    auth_message: &[u8],
    presented_signature: &[u8],
) -> bool {
    presented_signature.len() == 32
        && constant_time_eq(
            &server_signature(server_key, auth_message),
            presented_signature,
        )
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    // No early return on a length mismatch: the loop covers the longer
    // input and the length difference is folded into the result.
    let len = left.len().max(right.len());
    let mut difference = u8::from(left.len() != right.len());
    for i in 0..len {
        let l = left.get(i).copied().unwrap_or(0);
        let r = right.get(i).copied().unwrap_or(0);
        difference |= l ^ r;
    }
    std::hint::black_box(difference) == 0
}
