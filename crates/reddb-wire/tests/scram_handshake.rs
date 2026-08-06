use reddb_wire::redwire::handshake::base64_std;
use reddb_wire::redwire::scram::{
    ScramServerHandshake, ScramServerInput, ScramServerOutput, ScramVerifier,
};
use reddb_wire::redwire::MessageKind;

const CLIENT_NONCE: &str = "rOprNGfwEbeRWgbNEkqO";
const SERVER_NONCE: &str = "%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0";
const COMBINED_NONCE: &str = "rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0";
const SERVER_FIRST: &str = concat!(
    "r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,",
    "s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096"
);

#[test]
fn rfc_7677_success_exchange_authenticates_without_io() {
    let salt = reddb_wire::redwire::handshake::base64_std_decode("W22ZaJ0SNY7soEsUEjb6gQ==")
        .expect("RFC vector salt is valid base64");
    let verifier = ScramVerifier::from_password("pencil", salt, 4096);
    let mut handshake = ScramServerHandshake::new(SERVER_NONCE, vec![0; 16]);

    assert_eq!(
        handshake.step(client_message(
            1,
            format!("n,,n=user,r={CLIENT_NONCE}").into_bytes(),
        )),
        ScramServerOutput::NeedVerifier {
            username: "user".to_string(),
        }
    );
    assert_eq!(
        handshake.step(ScramServerInput::Verifier(Some(verifier))),
        ScramServerOutput::Challenge {
            correlation_id: 1,
            payload: SERVER_FIRST.as_bytes().to_vec(),
        }
    );

    let final_message =
        format!("c=biws,r={COMBINED_NONCE},p=dHzbZapWIk4jUhN+Ute9ytag9zjfMHgsqmmiz7AndVQ=");
    let output = handshake.step(client_message(2, final_message.into_bytes()));
    let ScramServerOutput::Authenticated {
        correlation_id,
        username,
        server_signature,
    } = &output
    else {
        panic!("RFC exchange should authenticate, got {output:?}");
    };
    assert_eq!(*correlation_id, 2);
    assert_eq!(username, "user");
    assert_eq!(
        base64_std(server_signature),
        "6rriTRBi23WpRR/wtup+mMhUZUn/dB5nLTJRsjl95G4="
    );
}

#[test]
fn wrong_proof_exchange_is_rejected_without_io() {
    let salt = reddb_wire::redwire::handshake::base64_std_decode("W22ZaJ0SNY7soEsUEjb6gQ==")
        .expect("RFC vector salt is valid base64");
    let verifier = ScramVerifier::from_password("pencil", salt, 4096);
    let mut handshake = ScramServerHandshake::new(SERVER_NONCE, vec![0; 16]);

    assert!(matches!(
        handshake.step(client_message(
            11,
            format!("n,,n=user,r={CLIENT_NONCE}").into_bytes(),
        )),
        ScramServerOutput::NeedVerifier { .. }
    ));
    assert!(matches!(
        handshake.step(ScramServerInput::Verifier(Some(verifier))),
        ScramServerOutput::Challenge { .. }
    ));

    let output = handshake.step(client_message(
        12,
        format!("c=biws,r={COMBINED_NONCE},p=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .into_bytes(),
    ));
    assert_eq!(
        output,
        ScramServerOutput::Failed {
            correlation_id: 12,
            reason: "invalid SCRAM proof".to_string(),
        }
    );
}

#[test]
fn malformed_exchange_is_rejected_without_io() {
    let salt = reddb_wire::redwire::handshake::base64_std_decode("W22ZaJ0SNY7soEsUEjb6gQ==")
        .expect("RFC vector salt is valid base64");
    let verifier = ScramVerifier::from_password("pencil", salt, 4096);
    let mut handshake = ScramServerHandshake::new(SERVER_NONCE, vec![0; 16]);

    assert!(matches!(
        handshake.step(client_message(
            21,
            format!("n,,n=user,r={CLIENT_NONCE}").into_bytes(),
        )),
        ScramServerOutput::NeedVerifier { .. }
    ));
    assert!(matches!(
        handshake.step(ScramServerInput::Verifier(Some(verifier))),
        ScramServerOutput::Challenge { .. }
    ));

    assert_eq!(
        handshake.step(client_message(
            22,
            format!("c=biws,r={COMBINED_NONCE}").into_bytes(),
        )),
        ScramServerOutput::Failed {
            correlation_id: 22,
            reason: "scram client-final: missing p=<proof>".to_string(),
        }
    );
}

fn client_message(correlation_id: u64, payload: Vec<u8>) -> ScramServerInput {
    ScramServerInput::ClientMessage {
        correlation_id,
        kind: MessageKind::AuthResponse,
        payload,
    }
}
