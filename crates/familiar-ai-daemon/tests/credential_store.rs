use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use familiar_ai_core::config::{AuthDescriptor, CredentialStoreDescriptor, InferenceRuntimeKind};
use familiar_ai_daemon::config_cli::{
    check_auth_with_store, probe_with_store, CredentialStore, CredentialStoreError,
};
use familiar_ai_daemon::preflight::{provider_auth_check_with_store, PreflightStatus};

const SECRET: &str = "credential-store-test-value";

struct FakeStore(Result<&'static str, CredentialStoreError>);

impl CredentialStore for FakeStore {
    fn resolve(
        &self,
        _descriptor: &CredentialStoreDescriptor,
    ) -> Result<String, CredentialStoreError> {
        self.0.map(str::to_owned)
    }
}

fn descriptor() -> AuthDescriptor {
    AuthDescriptor::try_from(
        "credential-store: macos-keychain/com.example.familiar/worker".to_owned(),
    )
    .unwrap()
}

#[test]
fn descriptor_round_trips_without_credential_material() {
    let auth = descriptor();
    let encoded = String::from(auth.clone());
    assert_eq!(
        encoded,
        "credential-store: macos-keychain/com.example.familiar/worker"
    );
    assert_eq!(AuthDescriptor::try_from(encoded.clone()).unwrap(), auth);

    let source = format!(
        "# retained comment\n[providers.remote]\nkind = \"inference\"\nhost = \"localhost:443\"\nauth = \"{encoded}\"\n"
    );
    let parsed: familiar_ai_core::Config = toml::from_str(&source).unwrap();
    assert_eq!(parsed.providers["remote"].auth, auth);
    assert!(source.contains("# retained comment"));
    assert!(!source.contains(SECRET));
}

#[test]
fn fake_store_resolves_in_memory_without_environment_export() {
    const BRIDGE: &str = "FAMILIAR_AI_CREDENTIAL_STORE_TEST_BRIDGE";
    std::env::remove_var(BRIDGE);
    let credential = check_auth_with_store(&descriptor(), &FakeStore(Ok(SECRET)))
        .unwrap()
        .unwrap();
    assert_eq!(credential.expose_for_request(), SECRET);
    assert_eq!(format!("{credential:?}"), "ResolvedCredential([REDACTED])");
    assert!(std::env::var_os(BRIDGE).is_none());
}

#[test]
fn missing_denied_and_unsupported_conditions_fail_closed_by_reference() {
    for (condition, expected) in [
        (CredentialStoreError::Missing, "missing or empty"),
        (CredentialStoreError::AccessDenied, "access was denied"),
        (
            CredentialStoreError::UnsupportedPlatform,
            "unsupported on this platform",
        ),
    ] {
        let error = check_auth_with_store(&descriptor(), &FakeStore(Err(condition))).unwrap_err();
        assert!(error.contains("credential-store: macos-keychain/com.example.familiar/worker"));
        assert!(error.contains(expected));
        assert!(!error.contains(SECRET));
    }
}

#[test]
fn supervisor_preflight_uses_store_without_a_login_shell() {
    let check = provider_auth_check_with_store("remote", &descriptor(), &FakeStore(Ok(SECRET)));
    assert_eq!(check.status, PreflightStatus::Passed);
    assert!(check.detail.contains("credential-store: macos-keychain"));
    assert!(!check.detail.contains(SECRET));
}

/// Loopback binding is forbidden inside the coding-agent sandbox, where it
/// surfaces as `Operation not permitted`. A fixture that requires the socket
/// must report that condition rather than fail as a product defect
/// (FAM-BUG-015) — verification classifies the same words as
/// `EnvironmentDenied`, and this keeps the fixture's own verdict honest.
fn loopback_listener_or_skip(test: &str) -> Option<TcpListener> {
    match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => Some(listener),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::AddrNotAvailable
            ) =>
        {
            eprintln!(
                "{test}: skipped — environment denied loopback bind (Operation not permitted): {error}"
            );
            None
        }
        Err(error) => panic!("unexpected loopback bind failure: {error}"),
    }
}

#[test]
fn provider_probe_uses_resolved_store_value_only_as_bearer_auth() {
    let Some(listener) =
        loopback_listener_or_skip("provider_probe_uses_resolved_store_value_only_as_bearer_auth")
    else {
        return;
    };
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let size = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..size]);
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer credential-store-test-value"));
        let body = r#"{"data":[{"id":"unsloth/test-model"}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let models = probe_with_store(
        Some(InferenceRuntimeKind::Unsloth),
        "remote",
        &address.to_string(),
        &descriptor(),
        &FakeStore(Ok(SECRET)),
    )
    .unwrap();
    server.join().unwrap();
    assert_eq!(models, ["unsloth/test-model"]);
}

#[test]
fn environment_reference_behavior_is_unchanged() {
    const NAME: &str = "FAMILIAR_AI_PRD_074_ENV_TEST";
    std::env::set_var(NAME, SECRET);
    let auth = AuthDescriptor::Env(NAME.into());
    let resolved = check_auth_with_store(&auth, &FakeStore(Err(CredentialStoreError::Missing)))
        .unwrap()
        .unwrap();
    assert_eq!(resolved.expose_for_request(), SECRET);
    std::env::remove_var(NAME);
}
