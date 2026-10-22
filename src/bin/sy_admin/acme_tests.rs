//! SY.admin's ACME client against a fake CA and a fake edge, vault and orchestrator
//! (docs/acme-edge-certificates-v1.md §10, "SY.admin"): the order flow, the decision table of §7,
//! the backoff and the cap, the conditions and their alerts, and the configuration checks.

use super::*;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, Ordering};

use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, SerialNumber,
};

const NAME: &str = "hive-1.example.com";
const EDGE: &str = "SY.edge@ingress1";
const EDGE_KEY: &str = "edge_tls";
const DIRECTORY: &str = "https://ca.example.test/directory";
const STAGING: &str = "https://staging.example.test/directory";
const DAY: i64 = 86_400;
const HOUR: i64 = 3_600;

fn account_url_for(directory: &str) -> String {
    format!("{}/acct/1", directory.trim_end_matches("/directory"))
}

fn admin_ilk() -> String {
    fluxbee_sdk::deterministic_system_ilk_id("SY.admin@motherbee")
}

fn edge_ilk() -> String {
    fluxbee_sdk::deterministic_system_ilk_id(EDGE)
}

fn set_validity(params: &mut CertificateParams, not_before: i64, not_after: i64) {
    let epoch = rcgen::date_time_ymd(1970, 1, 1);
    params.not_before = epoch + Duration::from_secs(not_before as u64);
    params.not_after = epoch + Duration::from_secs(not_after as u64);
}

/// A CA of our own: a root, an intermediate, leaves signed for a CSR or seeded by hand.
pub(super) struct TestPki {
    root_der: CertificateDer<'static>,
    root_pem: String,
    intermediate: CertifiedIssuer<'static, KeyPair>,
    serial: AtomicI64,
}

impl TestPki {
    pub(super) fn new(label: &str) -> Self {
        let mut root = CertificateParams::new(Vec::<String>::new()).expect("root params");
        root.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        root.distinguished_name = DistinguishedName::new();
        root.distinguished_name
            .push(DnType::CommonName, format!("{label} Root"));
        let root = CertifiedIssuer::self_signed(root, KeyPair::generate().expect("root key"))
            .expect("root");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, format!("{label} Intermediate"));
        params.use_authority_key_identifier_extension = true;
        let intermediate = CertifiedIssuer::signed_by(
            params,
            KeyPair::generate().expect("intermediate key"),
            &root,
        )
        .expect("intermediate");
        Self {
            root_der: root.der().clone(),
            root_pem: root.pem(),
            intermediate,
            serial: AtomicI64::new(1),
        }
    }

    fn next_serial(&self) -> SerialNumber {
        let serial = self.serial.fetch_add(1, Ordering::SeqCst);
        SerialNumber::from_slice(&[0x10, (serial % 250) as u8 + 1])
    }

    /// The leaf for a CSR, signed by the intermediate: the chain, PEM.
    fn issue(&self, csr_der: &[u8], not_before: i64, not_after: i64) -> String {
        let csr = rustls::pki_types::CertificateSigningRequestDer::from(csr_der.to_vec());
        let mut request =
            rcgen::CertificateSigningRequestParams::from_der(&csr).expect("the CSR parses");
        set_validity(&mut request.params, not_before, not_after);
        request.params.use_authority_key_identifier_extension = true;
        request.params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        request.params.serial_number = Some(self.next_serial());
        let leaf = request.signed_by(&self.intermediate).expect("the leaf");
        format!("{}{}", leaf.pem(), self.intermediate.pem())
    }

    /// A leaf with its own key, as an operator seeds one by hand: (chain, key), PEM.
    fn seeded(&self, names: &[&str], not_before: i64, not_after: i64) -> (String, String) {
        let key = KeyPair::generate().expect("key");
        let mut params =
            CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
                .expect("params");
        params.distinguished_name = DistinguishedName::new();
        set_validity(&mut params, not_before, not_after);
        params.use_authority_key_identifier_extension = true;
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.serial_number = Some(self.next_serial());
        let leaf = params.signed_by(&key, &self.intermediate).expect("leaf");
        (
            format!("{}{}", leaf.pem(), self.intermediate.pem()),
            key.serialize_pem(),
        )
    }
}

fn leaf_of(chain_pem: &str) -> LeafInfo {
    let (chain, _) = tls_material::parse_chain(chain_pem.as_bytes()).expect("chain");
    tls_material::leaf_info(chain[0].as_ref(), chain.len()).expect("leaf")
}

fn first_pem(chain_pem: &str) -> String {
    let end = "-----END CERTIFICATE-----";
    let at = chain_pem.find(end).expect("a certificate") + end.len();
    format!("{}\n", &chain_pem[..at])
}

// ---------------------------------------------------------------------------------------------
// The fake CA
// ---------------------------------------------------------------------------------------------

#[derive(Default, Debug)]
pub(super) struct CaLog {
    /// (name, profile, replaces sent)
    new_orders: Vec<(String, Option<String>, bool)>,
    resumed: Vec<String>,
    accounts_created: Vec<String>,
    directories: usize,
    ari_calls: usize,
    set_ready: usize,
    deactivated: usize,
    finalized: usize,
}

pub(super) struct CaScript {
    directory: DirectoryInfo,
    ari: Result<AriAnswer, CaError>,
    new_order_errors: VecDeque<CaError>,
    authz: AuthzState,
    poll: Result<OrderStatus, CaError>,
    challenge_error: Option<String>,
    resume_status: OrderStatus,
    lifetime: i64,
    /// Run once when the CA hands out a certificate: something that happens during an order.
    on_certificate: Option<Box<dyn FnOnce() + Send>>,
    log: CaLog,
}

impl Default for CaScript {
    fn default() -> Self {
        Self {
            directory: DirectoryInfo {
                terms_of_service: Some("https://ca.example.test/terms".to_string()),
                profiles: vec!["classic".to_string(), "tlsserver".to_string()],
                renewal_info: Some("https://ca.example.test/ari".to_string()),
            },
            ari: Err(CaError::Problem("no ARI in this test".to_string())),
            new_order_errors: VecDeque::new(),
            authz: AuthzState::Pending {
                name: NAME.to_string(),
                digest: "ab".repeat(32),
            },
            poll: Ok(OrderStatus::Ready),
            challenge_error: None,
            resume_status: OrderStatus::Pending,
            lifetime: 90 * DAY,
            on_certificate: None,
            log: CaLog::default(),
        }
    }
}

struct FakeCa {
    script: Arc<Mutex<CaScript>>,
    pki: Arc<TestPki>,
    clock: Arc<AtomicI64>,
}

#[async_trait]
impl AcmeCa for FakeCa {
    async fn directory(&self, _url: &str) -> Result<DirectoryInfo, CaError> {
        let mut script = lock(&self.script);
        script.log.directories += 1;
        Ok(script.directory.clone())
    }

    async fn renewal_info(&self, _url: &str, _cert: &CertRef) -> Result<AriAnswer, CaError> {
        let mut script = lock(&self.script);
        script.log.ari_calls += 1;
        script.ari.clone()
    }

    async fn create_account(
        &self,
        directory_url: &str,
        _contact: Option<&str>,
    ) -> Result<(Box<dyn CaAccount>, Value), CaError> {
        lock(&self.script)
            .log
            .accounts_created
            .push(directory_url.to_string());
        let url = account_url_for(directory_url);
        let credentials = json!({
            "id": url,
            "key_pkcs8": "c2VjcmV0LWFjY291bnQta2V5",
            "directory": directory_url,
        });
        Ok((self.account(&url), credentials))
    }

    async fn load_account(&self, credentials: &Value) -> Result<Box<dyn CaAccount>, CaError> {
        let url = credentials["id"].as_str().unwrap_or_default().to_string();
        Ok(self.account(&url))
    }
}

impl FakeCa {
    fn account(&self, url: &str) -> Box<dyn CaAccount> {
        Box::new(FakeAccount {
            url: url.to_string(),
            script: Arc::clone(&self.script),
            pki: Arc::clone(&self.pki),
            clock: Arc::clone(&self.clock),
        })
    }
}

struct FakeAccount {
    url: String,
    script: Arc<Mutex<CaScript>>,
    pki: Arc<TestPki>,
    clock: Arc<AtomicI64>,
}

#[async_trait]
impl CaAccount for FakeAccount {
    fn url(&self) -> String {
        self.url.clone()
    }

    fn profiles(&self) -> Vec<String> {
        lock(&self.script).directory.profiles.clone()
    }

    async fn new_order(
        &self,
        name: &str,
        profile: Option<&str>,
        replaces: Option<&CertRef>,
    ) -> Result<Box<dyn CaOrder>, CaError> {
        let mut script = lock(&self.script);
        script.log.new_orders.push((
            name.to_string(),
            profile.map(str::to_string),
            replaces.is_some(),
        ));
        if let Some(err) = script.new_order_errors.pop_front() {
            return Err(err);
        }
        let n = script.log.new_orders.len();
        Ok(self.order(format!("{}/order/{n}", self.url), OrderStatus::Pending))
    }

    async fn resume_order(&self, url: &str) -> Result<Box<dyn CaOrder>, CaError> {
        let status = {
            let mut script = lock(&self.script);
            script.log.resumed.push(url.to_string());
            script.resume_status
        };
        Ok(self.order(url.to_string(), status))
    }
}

impl FakeAccount {
    fn order(&self, url: String, status: OrderStatus) -> Box<dyn CaOrder> {
        Box::new(FakeOrder {
            url,
            status,
            csr: None,
            script: Arc::clone(&self.script),
            pki: Arc::clone(&self.pki),
            clock: Arc::clone(&self.clock),
        })
    }
}

struct FakeOrder {
    url: String,
    status: OrderStatus,
    csr: Option<Vec<u8>>,
    script: Arc<Mutex<CaScript>>,
    pki: Arc<TestPki>,
    clock: Arc<AtomicI64>,
}

#[async_trait]
impl CaOrder for FakeOrder {
    fn url(&self) -> String {
        self.url.clone()
    }

    fn status(&mut self) -> OrderStatus {
        self.status
    }

    async fn authorization(&mut self) -> Result<AuthzState, CaError> {
        Ok(lock(&self.script).authz.clone())
    }

    async fn set_challenge_ready(&mut self) -> Result<(), CaError> {
        lock(&self.script).log.set_ready += 1;
        Ok(())
    }

    async fn poll_ready(&mut self, _limit: Duration) -> Result<OrderStatus, CaError> {
        let polled = lock(&self.script).poll.clone();
        if let Ok(status) = &polled {
            self.status = *status;
        }
        polled
    }

    async fn challenge_error(&mut self) -> Option<String> {
        lock(&self.script).challenge_error.clone()
    }

    async fn deactivate_pending(&mut self) -> Result<(), CaError> {
        lock(&self.script).log.deactivated += 1;
        Ok(())
    }

    async fn finalize(&mut self, csr_der: &[u8]) -> Result<(), CaError> {
        lock(&self.script).log.finalized += 1;
        self.csr = Some(csr_der.to_vec());
        self.status = OrderStatus::Processing;
        Ok(())
    }

    async fn certificate(&mut self, _limit: Duration) -> Result<String, CaError> {
        let csr = self
            .csr
            .clone()
            .ok_or_else(|| CaError::Problem("not finalized".to_string()))?;
        let now = self.clock.load(Ordering::SeqCst);
        let (lifetime, hook) = {
            let mut script = lock(&self.script);
            (script.lifetime, script.on_certificate.take())
        };
        if let Some(hook) = hook {
            hook();
        }
        self.status = OrderStatus::Valid;
        Ok(self.pki.issue(&csr, now - 60, now - 60 + lifetime))
    }
}

// ---------------------------------------------------------------------------------------------
// The fake edge, vault and orchestrator
// ---------------------------------------------------------------------------------------------

/// One version of a secret: its value, its number, when it became current and who wrote it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Version {
    value: Value,
    number: i64,
    updated_at: String,
    created_by: String,
}

/// A secret as SY.vault keeps it: the current version and one previous. A rollback swaps the two
/// (numbers included) and stamps a new `updated_at`.
#[derive(Clone, Debug)]
pub(super) struct Secret {
    metadata: Value,
    current: Version,
    previous: Option<Version>,
}

pub(super) struct MeshScript {
    edge_mode: String,
    edge_key: String,
    /// EDGE_TLS_STATUS gets no answer, with this reason.
    edge_down: Option<String>,
    /// EDGE_TLS_STATUS answers `status: error` with this text.
    edge_error: Option<String>,
    reload_down: bool,
    /// The reload refuses the material with this detail, and `reload_refusal` as its reason.
    reload_refuses: Option<String>,
    reload_refusal: &'static str,
    /// The chain the edge serves and the vault write it came from; `None`: awaiting.
    served: Option<String>,
    served_write: Option<(i64, String)>,
    awaiting_reason: String,
    vault: HashMap<String, Secret>,
    /// SY.vault's clock for `updated_at`.
    ticks: u64,
    vault_down: bool,
    /// The next puts of a key that get no answer, and are not applied.
    put_failures: HashMap<String, usize>,
    /// The next puts of a key that are applied, but whose answer is lost.
    puts_unanswered: HashMap<String, usize>,
    alerts_down: bool,
    /// The orchestrator answers `recorded: false` (a duplicate of its newest row).
    alerts_duplicate: bool,
    /// VAULT_ROLLBACK gets no answer, and is not applied.
    rollback_down: bool,
    /// VAULT_ROLLBACK is applied, but its answer is lost.
    rollback_unanswered: bool,
    statuses: usize,
    reloads: usize,
    rollbacks: usize,
    arms: Vec<Value>,
    clears: Vec<Value>,
    alerts: Vec<Value>,
}

impl Default for MeshScript {
    fn default() -> Self {
        Self {
            edge_mode: "vault".to_string(),
            edge_key: EDGE_KEY.to_string(),
            edge_down: None,
            edge_error: None,
            reload_down: false,
            reload_refuses: None,
            reload_refusal: "invalid_material",
            served: None,
            served_write: None,
            awaiting_reason: "not_found".to_string(),
            vault: HashMap::new(),
            ticks: 0,
            vault_down: false,
            put_failures: HashMap::new(),
            puts_unanswered: HashMap::new(),
            alerts_down: false,
            alerts_duplicate: false,
            rollback_down: false,
            rollback_unanswered: false,
            statuses: 0,
            reloads: 0,
            rollbacks: 0,
            arms: Vec::new(),
            clears: Vec::new(),
            alerts: Vec::new(),
        }
    }
}

impl MeshScript {
    fn status_answer(&self) -> Value {
        let (vault_version, vault_updated_at) = match &self.served_write {
            Some((number, at)) => (json!(number), json!(at)),
            None => (Value::Null, Value::Null),
        };
        match &self.served {
            Some(chain) => {
                let leaf = leaf_of(chain);
                json!({
                    "status": "ok",
                    "mode": self.edge_mode,
                    "tls_vault_key": self.edge_key,
                    "state": "serving",
                    "reason": null,
                    "leaf_pem": first_pem(chain),
                    "fingerprint": leaf.fingerprint,
                    "not_before": leaf.not_before,
                    "not_after": leaf.not_after,
                    "names": leaf.names,
                    "issuer": leaf.issuer,
                    "chain_len": leaf.chain_len,
                    "loaded_at": 1,
                    "vault_version": vault_version,
                    "vault_updated_at": vault_updated_at,
                })
            }
            None => json!({
                "status": "ok",
                "mode": self.edge_mode,
                "tls_vault_key": self.edge_key,
                "state": "awaiting",
                "reason": self.awaiting_reason,
                "detail": format!("vault get '{}' failed", self.edge_key),
                "leaf_pem": null,
                "fingerprint": null,
                "not_before": null,
                "not_after": null,
                "names": null,
                "issuer": null,
                "chain_len": null,
                "loaded_at": null,
                "vault_version": null,
                "vault_updated_at": null,
            }),
        }
    }

    fn tick(&mut self) -> String {
        self.ticks += 1;
        format!("2026-10-21T00:00:00.{:06}Z", self.ticks)
    }

    /// A put, as SY.vault does it: the next number, a new `updated_at`, the old current kept as
    /// the previous version.
    fn put(&mut self, key: &str, value: Value, metadata: Value) -> i64 {
        let updated_at = self.tick();
        let old = self.vault.remove(key);
        let number = old.as_ref().map_or(1, |old| old.current.number + 1);
        self.vault.insert(
            key.to_string(),
            Secret {
                metadata,
                current: Version {
                    value,
                    number,
                    updated_at,
                    created_by: admin_ilk(),
                },
                previous: old.map(|old| old.current),
            },
        );
        number
    }

    /// A rollback, as SY.vault does it: the two versions swap, numbers included, and the
    /// metadata's `updated_at` is now.
    fn rollback(&mut self, key: &str) -> Result<(), MeshError> {
        let updated_at = self.tick();
        let Some(secret) = self.vault.get_mut(key) else {
            return Err(MeshError::Refused {
                code: "KEY_NOT_FOUND".to_string(),
                detail: key.to_string(),
            });
        };
        let Some(mut previous) = secret.previous.take() else {
            return Err(MeshError::Refused {
                code: "NO_PREVIOUS_VERSION".to_string(),
                detail: key.to_string(),
            });
        };
        previous.updated_at = updated_at;
        secret.previous = Some(std::mem::replace(&mut secret.current, previous));
        Ok(())
    }

    fn edge_metadata() -> Value {
        json!({
            "tenant_id": DEFAULT_ROOT_TENANT_ID,
            "resource_type": "tls",
            "ilk": edge_ilk(),
        })
    }

    /// Seed the edge's secret, as an operator does by hand, and have the edge serve it.
    fn seed(&mut self, chain: &str, key: &str) {
        let edge_key = self.edge_key.clone();
        self.put(
            &edge_key,
            json!({ "cert": chain, "key": key }),
            Self::edge_metadata(),
        );
        self.serve_current();
    }

    /// The edge loads the vault's current version of its secret.
    fn serve_current(&mut self) {
        match self.vault.get(&self.edge_key) {
            Some(secret) => {
                self.served = secret.current.value["cert"].as_str().map(str::to_string);
                self.served_write =
                    Some((secret.current.number, secret.current.updated_at.clone()));
            }
            None => {
                self.served = None;
                self.served_write = None;
            }
        }
    }

    fn secret_cert(&self) -> Option<String> {
        self.vault
            .get(&self.edge_key)
            .and_then(|secret| secret.current.value["cert"].as_str())
            .map(str::to_string)
    }
}

struct FakeMesh {
    script: Arc<Mutex<MeshScript>>,
}

#[async_trait]
impl AcmeMesh for FakeMesh {
    async fn edge_status(&self, _edge: &str, _limit: Duration) -> Result<Value, MeshError> {
        let mut script = lock(&self.script);
        script.statuses += 1;
        if let Some(reason) = &script.edge_down {
            return Err(MeshError::NoAnswer(reason.clone()));
        }
        if let Some(error) = &script.edge_error {
            return Ok(json!({ "status": "error", "error": error }));
        }
        Ok(script.status_answer())
    }

    async fn edge_reload(&self, _edge: &str, _limit: Duration) -> Result<Value, MeshError> {
        let mut script = lock(&self.script);
        script.reloads += 1;
        if script.reload_down || script.edge_down.is_some() {
            return Err(MeshError::NoAnswer("timed out".to_string()));
        }
        let Some(cert) = script.secret_cert() else {
            let mut answer = script.status_answer();
            answer["changed"] = json!(false);
            answer["refused"] = json!({ "reason": "not_found", "detail": "KEY_NOT_FOUND" });
            return Ok(answer);
        };
        if let Some(detail) = script.reload_refuses.clone() {
            // A refusal keeps the current certificate and is still `status: "ok"`.
            let mut answer = script.status_answer();
            answer["changed"] = json!(false);
            answer["refused"] = json!({ "reason": script.reload_refusal, "detail": detail });
            return Ok(answer);
        }
        let changed = script.served.as_deref() != Some(cert.as_str());
        script.serve_current();
        let mut answer = script.status_answer();
        answer["changed"] = json!(changed);
        Ok(answer)
    }

    async fn edge_challenge(
        &self,
        _edge: &str,
        request: Value,
        _limit: Duration,
    ) -> Result<Value, MeshError> {
        let mut script = lock(&self.script);
        let clear = request["clear"].as_bool() == Some(true);
        let answer = json!({
            "status": "ok",
            "name": request["name"],
            "armed": !clear,
            "expires_at": 600,
        });
        if clear {
            script.clears.push(request);
        } else {
            script.arms.push(request);
        }
        Ok(answer)
    }

    async fn vault_metadata(
        &self,
        key: &str,
        _limit: Duration,
    ) -> Result<Option<SecretMetadata>, MeshError> {
        let script = lock(&self.script);
        if script.vault_down {
            return Err(MeshError::NoAnswer("SY.vault timed out".to_string()));
        }
        Ok(script.vault.get(key).map(|secret| {
            let text = |field: &str| secret.metadata[field].as_str().map(str::to_string);
            SecretMetadata {
                resource_type: text("resource_type"),
                tenant_id: text("tenant_id"),
                owner_ilk: text("ilk"),
                version: Some(secret.current.number),
                updated_at: Some(secret.current.updated_at.clone()),
                created_by: Some(secret.current.created_by.clone()),
            }
        }))
    }

    async fn vault_value(&self, key: &str, _limit: Duration) -> Result<Option<Value>, MeshError> {
        let script = lock(&self.script);
        if script.vault_down {
            return Err(MeshError::NoAnswer("SY.vault timed out".to_string()));
        }
        Ok(script
            .vault
            .get(key)
            .map(|secret| secret.current.value.clone()))
    }

    async fn vault_put(
        &self,
        key: &str,
        value: Value,
        metadata: Value,
        _limit: Duration,
    ) -> Result<Option<i64>, MeshError> {
        let mut script = lock(&self.script);
        if script.vault_down {
            return Err(MeshError::NoAnswer("SY.vault timed out".to_string()));
        }
        if let Some(failures) = script.put_failures.get_mut(key) {
            if *failures > 0 {
                *failures -= 1;
                return Err(MeshError::NoAnswer("SY.vault timed out".to_string()));
            }
        }
        let number = script.put(key, value, metadata);
        if let Some(unanswered) = script.puts_unanswered.get_mut(key) {
            if *unanswered > 0 {
                *unanswered -= 1;
                return Err(MeshError::NoAnswer("SY.vault timed out".to_string()));
            }
        }
        Ok(Some(number))
    }

    async fn vault_rollback(&self, key: &str, _limit: Duration) -> Result<(), MeshError> {
        let mut script = lock(&self.script);
        if script.vault_down || script.rollback_down {
            return Err(MeshError::NoAnswer("SY.vault timed out".to_string()));
        }
        script.rollbacks += 1;
        script.rollback(key)?;
        if script.rollback_unanswered {
            return Err(MeshError::NoAnswer("SY.vault timed out".to_string()));
        }
        Ok(())
    }

    async fn drift_alert(&self, alert: Value, _limit: Duration) -> Result<bool, MeshError> {
        let mut script = lock(&self.script);
        if script.alerts_down {
            return Err(MeshError::NoAnswer("SY.orchestrator timed out".to_string()));
        }
        script.alerts.push(alert);
        Ok(!script.alerts_duplicate)
    }
}

// ---------------------------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------------------------

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "sy-admin-acme-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn settings() -> AcmeSettings {
    AcmeSettings {
        name: NAME.to_string(),
        directory_url: DIRECTORY.to_string(),
        staging_directory_url: STAGING.to_string(),
        profile: None,
        contact: Some("mailto:ops@example.com".to_string()),
        trust_anchor: None,
        trust_anchor_roots: Vec::new(),
    }
}

fn test_timings() -> Timings {
    Timings {
        first_check: Duration::from_millis(1),
        check_interval: Duration::from_secs(6 * 3_600),
        check_jitter: Duration::from_secs(30 * 60),
        edge_retry: Duration::from_secs(600),
        ca_call: Duration::from_secs(5),
        order_poll: Duration::from_secs(5),
        edge_call: Duration::from_secs(5),
        reload: Duration::from_secs(5),
        mesh_call: Duration::from_secs(5),
        vault_retry_window: Duration::from_millis(300),
        vault_retry_every: Duration::from_millis(5),
        directory_refresh: Duration::from_secs(86_400),
    }
}

struct Harness {
    service: Arc<AcmeService>,
    config: AcmeConfig,
    ca: Arc<Mutex<CaScript>>,
    mesh: Arc<Mutex<MeshScript>>,
    clock: Arc<AtomicI64>,
    pki: Arc<TestPki>,
    platform: Arc<TestPki>,
    dir: PathBuf,
}

impl Harness {
    fn new(config: AcmeConfig) -> Self {
        let pki = Arc::new(TestPki::new("Test"));
        let clock = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp()));
        let mut harness = Self {
            service: inert_service(&temp_dir("placeholder")),
            config,
            ca: Arc::new(Mutex::new(CaScript::default())),
            mesh: Arc::new(Mutex::new(MeshScript::default())),
            clock,
            platform: Arc::clone(&pki),
            pki,
            dir: temp_dir("harness"),
        };
        harness.restart();
        harness
    }

    fn on() -> Self {
        Self::new(AcmeConfig::On(settings()))
    }

    fn absent() -> Self {
        Self::new(AcmeConfig::Absent {
            name: Some(NAME.to_string()),
        })
    }

    /// A new service over the same CA, mesh, clock and acme.json: a restart of SY.admin.
    fn restart(&mut self) {
        let clock = Arc::clone(&self.clock);
        let platform = Arc::clone(&self.platform);
        let deps = AcmeDeps {
            ca: Arc::new(FakeCa {
                script: Arc::clone(&self.ca),
                pki: Arc::clone(&self.pki),
                clock: Arc::clone(&self.clock),
            }),
            mesh: Arc::new(FakeMesh {
                script: Arc::clone(&self.mesh),
            }),
            edge: Arc::new(|| Ok(EDGE.to_string())),
            clock: Arc::new(move || clock.load(Ordering::SeqCst)),
            random: Arc::new(|| 0.5),
            platform_roots: Arc::new(move || Ok(vec![platform.root_der.clone()])),
            timings: test_timings(),
        };
        self.service = AcmeService::new(
            self.config.clone(),
            deps,
            self.state_path(),
            admin_ilk(),
            "motherbee".to_string(),
        );
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("sy-admin/acme.json")
    }

    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn advance(&self, secs: i64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    fn state(&self) -> AcmeState {
        self.service.read(Clone::clone)
    }

    fn mesh(&self) -> MutexGuard<'_, MeshScript> {
        lock(&self.mesh)
    }

    fn ca(&self) -> MutexGuard<'_, CaScript> {
        lock(&self.ca)
    }

    /// The (kind, state) of every alert written so far.
    fn alerts(&self) -> Vec<(String, String)> {
        self.mesh()
            .alerts
            .iter()
            .map(|alert| {
                (
                    alert["kind"].as_str().unwrap_or_default().to_string(),
                    alert["state"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    fn open_conditions(&self) -> Vec<String> {
        self.state().conditions.keys().cloned().collect()
    }

    /// Wait until the run a POST started is over.
    async fn settle(&self) {
        for _ in 0..1_000 {
            if lock(&self.service.slot).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("the ACME run did not finish");
    }

    /// Install a certificate through a first check (the edge starts with none).
    async fn install_first(&self) -> String {
        self.service.check_once().await;
        let state = self.state();
        assert_eq!(
            state.last_attempt.as_ref().map(|a| a.result.as_str()),
            Some("installed"),
            "{:?}",
            state.last_attempt
        );
        state.issued.last().expect("issued").fingerprint.clone()
    }
}

/// A service that talks to nothing, for the admin's own tests.
pub(crate) fn inert_service(state_dir: &Path) -> Arc<AcmeService> {
    let pki = Arc::new(TestPki::new("Inert"));
    let clock = Arc::new(AtomicI64::new(chrono::Utc::now().timestamp()));
    let now = Arc::clone(&clock);
    AcmeService::new(
        AcmeConfig::Absent { name: None },
        AcmeDeps {
            ca: Arc::new(FakeCa {
                script: Arc::new(Mutex::new(CaScript::default())),
                pki,
                clock,
            }),
            mesh: Arc::new(FakeMesh {
                script: Arc::new(Mutex::new(MeshScript::default())),
            }),
            edge: Arc::new(|| Err("no ingress in this test".to_string())),
            clock: Arc::new(move || now.load(Ordering::SeqCst)),
            random: Arc::new(|| 0.5),
            platform_roots: Arc::new(|| Err("no roots in this test".to_string())),
            timings: test_timings(),
        },
        state_dir.join("sy-admin/acme.json"),
        admin_ilk(),
        "motherbee".to_string(),
    )
}

// ---------------------------------------------------------------------------------------------
// The order flow (§6)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_edge_with_no_certificate_gets_one_ordered_stored_and_served() {
    let h = Harness::on();
    let now = h.now();
    let next = h.service.check_once().await;

    let mesh = h.mesh();
    // The edge's secret, with explicit metadata: tls, root tenant, owned by the edge.
    let secret = mesh.vault.get(EDGE_KEY).expect("the edge's secret");
    assert_eq!(secret.metadata["resource_type"], "tls");
    assert_eq!(secret.metadata["tenant_id"], DEFAULT_ROOT_TENANT_ID);
    assert_eq!(secret.metadata["ilk"], edge_ilk());
    assert!(secret.current.value["key"]
        .as_str()
        .unwrap()
        .contains("PRIVATE KEY"));
    let leaf = leaf_of(secret.current.value["cert"].as_str().unwrap());
    assert_eq!(leaf.names, vec![NAME.to_string()]);
    assert_eq!(leaf.chain_len, 2);
    // The account, under its key, owned by SY.admin, with the directory in the description.
    let account_key = account_vault_key(DIRECTORY);
    assert!(account_key.starts_with("acme_account:") && account_key.len() == 13 + 16);
    let account = mesh.vault.get(&account_key).expect("the account");
    assert_eq!(account.metadata["resource_type"], "acme_account");
    assert_eq!(account.metadata["ilk"], admin_ilk());
    assert_eq!(account.metadata["tenant_id"], DEFAULT_ROOT_TENANT_ID);
    assert_eq!(account.metadata["description"], DIRECTORY);
    // The challenge was armed for 600 s and cleared by name and digest.
    assert_eq!(mesh.arms.len(), 1);
    assert_eq!(mesh.arms[0]["name"], NAME);
    assert_eq!(mesh.arms[0]["ttl_secs"], 600);
    assert_eq!(mesh.clears.len(), 1);
    assert_eq!(mesh.clears[0]["digest"], mesh.arms[0]["digest"]);
    assert_eq!(mesh.clears[0]["clear"], true);
    // The edge serves the new leaf.
    assert_eq!(
        mesh.served
            .as_deref()
            .map(|chain| leaf_of(chain).fingerprint),
        Some(leaf.fingerprint.clone())
    );
    assert!(mesh.alerts.is_empty(), "{:?}", mesh.alerts);
    drop(mesh);

    let state = h.state();
    assert_eq!(
        state.accounts[DIRECTORY].account_url,
        account_url_for(DIRECTORY)
    );
    let issued = &state.issued[0];
    assert!(issued.stored && issued.installed);
    assert_eq!(issued.fingerprint, leaf.fingerprint);
    assert_eq!(issued.directory_url, DIRECTORY);
    assert!(issued.authority_key_id.is_some());
    assert!(state.order.is_none() && state.pending_install.is_none());
    assert_eq!(state.last_attempt.unwrap().result, "installed");
    assert_eq!(state.backoff, Backoff::default());
    assert!(!h.ca().log.new_orders[0].2, "nothing to replace");
    // The next check is the regular one: 6 h and half the 30 min jitter.
    assert_eq!(next, now + 6 * HOUR + 900);

    // acme.json holds no key.
    let written = fs::read_to_string(h.state_path()).expect("acme.json");
    assert!(!written.contains("PRIVATE KEY") && !written.contains("key_pkcs8"));
    assert!(written.contains(&leaf.fingerprint));
}

#[tokio::test]
async fn a_challenge_the_ca_refuses_fails_backs_off_and_alerts_once() {
    let h = Harness::on();
    h.ca().poll = Ok(OrderStatus::Invalid);
    h.ca().challenge_error = Some("Connection refused by 203.0.113.7:443".to_string());
    let now = h.now();
    h.service.check_once().await;

    let state = h.state();
    let attempt = state.last_attempt.clone().unwrap();
    assert_eq!(attempt.result, "failed");
    assert_eq!(attempt.stage.as_deref(), Some("challenge"));
    assert!(attempt.detail.unwrap().contains("Connection refused"));
    assert_eq!(state.backoff.failures, 1);
    assert_eq!(state.backoff.next_attempt_at, Some(now + HOUR));
    assert!(state.order.is_none(), "an abandoned order is forgotten");
    assert!(!h.mesh().vault.contains_key(EDGE_KEY), "nothing stored");
    assert_eq!(
        h.ca().log.deactivated,
        1,
        "its authorization is deactivated"
    );
    assert_eq!(h.mesh().clears.len(), 1, "the challenge is cleared");
    let mut alerts = h.alerts();
    alerts.sort();
    assert_eq!(
        alerts,
        vec![
            ("edge_without_certificate".to_string(), "open".to_string()),
            ("renewal_failed".to_string(), "open".to_string()),
        ]
    );
    let critical = h
        .mesh()
        .alerts
        .iter()
        .find(|a| a["kind"] == "edge_without_certificate")
        .cloned()
        .unwrap();
    assert_eq!(critical["severity"], "critical");
    assert_eq!(critical["category"], "certificate");
    assert_eq!(critical["hive"], "ingress1");

    // Inside the backoff: no order, and the standing conditions write nothing.
    h.advance(10 * 60);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 1);
    assert_eq!(h.alerts().len(), 2);

    // Past it, the order goes through and both conditions close with one info row each.
    h.advance(HOUR);
    h.ca().poll = Ok(OrderStatus::Ready);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 2);
    assert_eq!(h.state().backoff, Backoff::default());
    let closed: Vec<Value> = h
        .mesh()
        .alerts
        .iter()
        .filter(|a| a["state"] == "closed")
        .cloned()
        .collect();
    assert_eq!(closed.len(), 2);
    assert!(closed.iter().all(|a| a["severity"] == "info"));
    h.service.check_once().await;
    assert_eq!(h.alerts().len(), 4, "a closed condition writes once");
}

#[tokio::test]
async fn a_poll_that_never_ends_abandons_the_order() {
    let h = Harness::on();
    h.ca().poll = Err(CaError::Timeout("still pending".to_string()));
    h.service.check_once().await;
    let state = h.state();
    assert_eq!(
        state.last_attempt.unwrap().stage.as_deref(),
        Some("challenge")
    );
    assert_eq!(h.ca().log.deactivated, 1);
    assert_eq!(h.mesh().clears.len(), 1);
    assert!(state.order.is_none());
}

#[tokio::test]
async fn an_install_the_edge_refuses_is_rolled_back_and_holds_orders_until_the_operator_acts() {
    let h = Harness::on();
    let now = h.now();
    let (chain, key) = h
        .pki
        .seeded(&["*.example.com"], now - 10 * DAY, now + 80 * DAY);
    h.mesh().seed(&chain, &key);
    h.mesh().reload_refuses = Some("the chain is truncated".to_string());

    h.service.begin_renew().expect("renew starts");
    h.settle().await;

    assert_eq!(h.mesh().rollbacks, 1);
    assert_eq!(
        h.mesh().secret_cert().as_deref(),
        Some(chain.as_str()),
        "the secret is rolled back to the hand-seeded certificate"
    );
    let state = h.state();
    assert_eq!(state.last_attempt.as_ref().unwrap().result, "refused");
    let refused = state.install_refused.clone().expect("held");
    assert!(refused.rolled_back);
    assert!(state.pending_install.is_none());
    assert!(state.rollback_pending.is_none());
    let mut alerts = h.alerts();
    alerts.sort();
    assert_eq!(
        alerts,
        vec![
            ("install_refused".to_string(), "open".to_string()),
            ("renewal_failed".to_string(), "open".to_string()),
        ]
    );

    // Even an edge with no certificate gets no order while the hold stands.
    {
        let mut mesh = h.mesh();
        mesh.vault.remove(EDGE_KEY);
        mesh.serve_current();
    }
    h.advance(2 * DAY);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 1);
    assert!(h
        .state()
        .last_check
        .unwrap()
        .decision
        .contains("until the operator acts"));

    // The operator's POST /acme/renew lifts it.
    h.mesh().reload_refuses = None;
    h.service.begin_renew().expect("renew starts");
    h.settle().await;
    let state = h.state();
    assert_eq!(state.last_attempt.unwrap().result, "installed");
    assert!(state.install_refused.is_none());
    h.service.check_once().await;
    assert!(h.open_conditions().is_empty(), "{:?}", h.open_conditions());
}

#[tokio::test]
async fn a_rollback_that_does_not_go_through_is_retried_and_holds_renewals() {
    let h = Harness::on();
    let now = h.now();
    let (chain, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&chain, &key);
    h.mesh().reload_refuses = Some("key mismatch".to_string());
    h.mesh().rollback_down = true;
    h.service.begin_renew().expect("renew starts");
    h.settle().await;
    let state = h.state();
    assert!(state.rollback_pending.is_some());
    assert!(!state.install_refused.as_ref().unwrap().rolled_back);
    assert_eq!(
        h.service.begin_renew().unwrap_err().error_code,
        "ROLLBACK_PENDING",
        "nothing is written while the edge does not serve the last version written"
    );
    // The next check retries it.
    h.mesh().rollback_down = false;
    h.service.check_once().await;
    let state = h.state();
    assert!(state.rollback_pending.is_none());
    assert!(state.install_refused.unwrap().rolled_back);
    assert_eq!(h.mesh().secret_cert().as_deref(), Some(chain.as_str()));
}

#[tokio::test]
async fn a_refused_first_certificate_has_nothing_to_roll_back_to() {
    let h = Harness::on();
    h.mesh().reload_refuses = Some("the leaf is not yet valid".to_string());
    h.service.check_once().await;
    let state = h.state();
    assert_eq!(state.last_attempt.unwrap().result, "refused");
    let refused = state.install_refused.expect("held");
    assert!(!refused.rolled_back);
    assert!(state.rollback_pending.is_none(), "nothing to retry");
    assert!(h.open_conditions().contains(&"install_refused".to_string()));
    // The vault now holds a version the edge never served (it still refuses it): nothing is
    // written over it.
    h.advance(DAY);
    h.service.begin_renew().expect("renew");
    h.settle().await;
    let attempt = h.state().last_attempt.unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("look"));
    assert!(attempt.detail.unwrap().contains("does not serve"));
    assert_eq!(h.ca().log.new_orders.len(), 1);
    // The operator deletes it, and renews.
    {
        let mut mesh = h.mesh();
        mesh.vault.remove(EDGE_KEY);
        mesh.serve_current();
        mesh.reload_refuses = None;
    }
    h.service.begin_renew().expect("renew");
    h.settle().await;
    assert_eq!(h.state().last_attempt.unwrap().result, "installed");
}

#[tokio::test]
async fn a_reload_with_no_answer_keeps_the_certificate_and_the_next_check_reloads_it() {
    let h = Harness::on();
    h.mesh().reload_down = true;
    let now = h.now();
    let next = h.service.check_once().await;
    let state = h.state();
    let pending = state.pending_install.clone().expect("stored, not served");
    assert_eq!(state.last_attempt.as_ref().unwrap().result, "not_served");
    assert!(!state.last_attempt.as_ref().unwrap().failed());
    assert_eq!(state.backoff.failures, 0);
    assert_eq!(next, now + 600, "the reload is retried in 10 min");
    assert!(h.mesh().vault.contains_key(EDGE_KEY));

    // The edge is back: the next check reloads, and orders nothing.
    h.mesh().reload_down = false;
    h.advance(600);
    h.service.check_once().await;
    let state = h.state();
    assert!(state.pending_install.is_none());
    assert_eq!(h.ca().log.new_orders.len(), 1, "never a new order");
    assert_eq!(state.last_attempt.unwrap().result, "installed");
    assert!(state
        .issued
        .iter()
        .any(|r| r.fingerprint == pending.fingerprint && r.installed));
    // POST /acme/renew refuses while a certificate waits; POST /acme/reload is the way.
    h.service
        .update(|state| state.pending_install = Some(pending));
    assert_eq!(
        h.service.begin_renew().unwrap_err().error_code,
        "CERTIFICATE_NOT_SERVED"
    );
}

#[tokio::test]
async fn a_transient_refusal_is_no_answer_nothing_is_rolled_back() {
    let h = Harness::on();
    h.mesh().reload_refuses = Some("vault get 'edge_tls' failed: timed out".to_string());
    h.mesh().reload_refusal = "transient";
    h.service.check_once().await;
    let state = h.state();
    assert_eq!(state.last_attempt.unwrap().result, "not_served");
    assert!(state.pending_install.is_some());
    assert!(state.install_refused.is_none());
    assert_eq!(h.mesh().rollbacks, 0);
    // The operator's reload says so, and leaves the stored certificate waiting.
    let refusal = h.service.reload_action().await.unwrap_err();
    assert_eq!(refusal.http_status, 503);
    assert_eq!(refusal.error_code, "EDGE_VAULT_UNAVAILABLE");
    assert!(h.state().pending_install.is_some());
    assert_eq!(h.mesh().rollbacks, 0);
    // The vault answers again: the next check installs it, with no new order.
    h.mesh().reload_refuses = None;
    h.advance(600);
    h.service.check_once().await;
    assert!(h.state().pending_install.is_none());
    assert_eq!(h.ca().log.new_orders.len(), 1);
}

#[tokio::test]
async fn the_reload_action_installs_a_waiting_certificate_and_reports_a_refusal() {
    let h = Harness::on();
    h.mesh().reload_down = true;
    h.service.check_once().await;
    assert!(h.state().pending_install.is_some());
    h.mesh().reload_down = false;
    let answer = h.service.reload_action().await.expect("reloaded");
    assert_eq!(answer["edge_node"], EDGE);
    assert!(h.state().pending_install.is_none());
    assert_eq!(h.state().last_attempt.unwrap().result, "installed");

    h.mesh().reload_refuses = Some("expired".to_string());
    let refusal = h.service.reload_action().await.unwrap_err();
    assert_eq!(refusal.http_status, 422);
    assert_eq!(refusal.error_code, "EDGE_REFUSED");
    h.mesh().edge_down = Some("timed out".to_string());
    assert_eq!(
        h.service.reload_action().await.unwrap_err().error_code,
        "EDGE_NO_ANSWER"
    );
}

#[tokio::test]
async fn a_vault_outage_after_finalize_is_retried_in_memory() {
    let h = Harness::on();
    h.mesh().put_failures.insert(EDGE_KEY.to_string(), 3);
    h.service.check_once().await;
    let state = h.state();
    assert_eq!(state.last_attempt.unwrap().result, "installed");
    assert!(state.issued[0].stored);
}

#[tokio::test]
async fn a_vault_outage_that_outlasts_the_retry_drops_the_certificate() {
    let h = Harness::on();
    h.mesh()
        .put_failures
        .insert(EDGE_KEY.to_string(), usize::MAX);
    h.service.check_once().await;
    let state = h.state();
    let attempt = state.last_attempt.unwrap();
    assert_eq!(attempt.result, "failed");
    assert_eq!(attempt.stage.as_deref(), Some("store"));
    assert_eq!(
        state.issued.len(),
        1,
        "recorded, though the cap does not count it"
    );
    assert!(!state.issued[0].stored);
    assert!(state.pending_install.is_none());
    assert_eq!(state.backoff.failures, 1);
    assert!(!h.mesh().vault.contains_key(EDGE_KEY));
}

fn seed_account_and_order(h: &Harness, order_url: &str) {
    h.mesh().put(
        &account_vault_key(DIRECTORY),
        json!({ "id": account_url_for(DIRECTORY) }),
        json!({}),
    );
    let now = h.now();
    h.service.update(|state| {
        state.accounts.insert(
            DIRECTORY.to_string(),
            AccountRecord {
                account_url: account_url_for(DIRECTORY),
                vault_key: account_vault_key(DIRECTORY),
                recorded_at: now,
            },
        );
        state.order = Some(OrderRecord {
            directory_url: DIRECTORY.to_string(),
            account_url: account_url_for(DIRECTORY),
            order_url: order_url.to_string(),
            name: NAME.to_string(),
            profile: None,
            replaces: false,
            kind: RunKind::Check,
            started_at: now,
        });
    });
}

#[tokio::test]
async fn an_order_recorded_before_a_restart_is_resumed_not_placed_again() {
    let mut h = Harness::on();
    let order_url = format!("{}/order/7", account_url_for(DIRECTORY));
    seed_account_and_order(&h, &order_url);
    h.restart();
    assert_eq!(
        h.state().order.as_ref().map(|o| o.order_url.clone()),
        Some(order_url.clone())
    );
    h.service.check_once().await;
    assert_eq!(h.ca().log.resumed, vec![order_url]);
    assert!(h.ca().log.new_orders.is_empty());
    assert!(h.ca().log.accounts_created.is_empty());
    assert_eq!(h.state().last_attempt.unwrap().result, "installed");
}

#[tokio::test]
async fn a_recorded_order_past_finalize_is_replaced_by_a_new_one() {
    let mut h = Harness::on();
    let order_url = format!("{}/order/7", account_url_for(DIRECTORY));
    seed_account_and_order(&h, &order_url);
    h.ca().resume_status = OrderStatus::Valid;
    h.restart();
    h.service.check_once().await;
    assert_eq!(h.ca().log.resumed.len(), 1);
    assert_eq!(
        h.ca().log.new_orders.len(),
        1,
        "its key died with the last run"
    );
    assert_eq!(h.state().last_attempt.unwrap().result, "installed");
}

#[tokio::test]
async fn a_refused_replaces_is_retried_once_without_it() {
    let h = Harness::on();
    h.install_first().await;
    // Past two thirds of the lifetime (and the cap): the leaf this account issued is due.
    h.advance(61 * DAY);
    h.ca().new_order_errors.push_back(CaError::Replaces(
        "urn:ietf:params:acme:error:alreadyReplaced".to_string(),
    ));
    h.service.check_once().await;
    let orders = h.ca().log.new_orders.clone();
    assert_eq!(orders.len(), 3);
    assert!(orders[1].2, "the renewal names the leaf it replaces");
    assert!(!orders[2].2, "and is placed once more without it");
    assert_eq!(h.state().last_attempt.unwrap().result, "installed");
}

#[tokio::test]
async fn only_a_leaf_this_account_issued_is_replaced() {
    let h = Harness::on();
    let fingerprint = h.install_first().await;
    // The same leaf recorded for another account is not this account's.
    h.service.update(|state| {
        for record in state.issued.iter_mut() {
            record.account_url = "https://ca.example.test/acct/other".to_string();
        }
    });
    h.advance(61 * DAY);
    h.service.check_once().await;
    assert_eq!(
        h.ca().log.new_orders.len(),
        1,
        "a foreign leaf is alerted only"
    );
    assert!(h
        .open_conditions()
        .contains(&"foreign_certificate".to_string()));
    // A forced renewal replaces it, without `replaces`.
    h.service.begin_renew().expect("renew");
    h.settle().await;
    let orders = h.ca().log.new_orders.clone();
    assert_eq!(orders.len(), 2);
    assert!(!orders[1].2);
    assert_ne!(h.state().issued.last().unwrap().fingerprint, fingerprint);
}

#[tokio::test]
async fn one_order_at_a_time_a_busy_slot_answers_409() {
    let h = Harness::on();
    let slot = h.service.try_begin(RunKind::Renew).expect("the slot");
    for refusal in [
        h.service.begin_renew().unwrap_err(),
        h.service.begin_test().unwrap_err(),
        h.service.reload_action().await.unwrap_err(),
    ] {
        assert_eq!(refusal.http_status, 409);
        assert_eq!(refusal.error_code, "ACME_BUSY");
        assert_eq!(refusal.payload["in_flight"]["kind"], "renew");
    }
    let now = h.now();
    assert_eq!(h.service.check_once().await, now + 600);
    assert_eq!(h.mesh().statuses, 0, "a busy check does nothing");
    drop(slot);
    h.service.begin_renew().expect("free again");
    h.settle().await;
}

// ---------------------------------------------------------------------------------------------
// The decision table (§7)
// ---------------------------------------------------------------------------------------------

fn view_serving(leaf: &LeafInfo) -> EdgeView {
    EdgeView {
        mode: Some("vault".to_string()),
        tls_vault_key: Some(EDGE_KEY.to_string()),
        state: Some("serving".to_string()),
        reason: None,
        detail: None,
        leaf: Some(LeafView {
            fingerprint: leaf.fingerprint.clone(),
            not_before: leaf.not_before,
            not_after: leaf.not_after,
            names: leaf.names.clone(),
            issuer: Some(leaf.issuer.clone()),
            chain_len: Some(leaf.chain_len),
        }),
        loaded_at: None,
        changed: None,
        refused: None,
        vault_version: Some(1),
        vault_updated_at: Some("2026-10-21T00:00:00Z".to_string()),
    }
}

fn view_awaiting(reason: &str) -> EdgeView {
    EdgeView {
        mode: Some("vault".to_string()),
        tls_vault_key: Some(EDGE_KEY.to_string()),
        state: Some("awaiting".to_string()),
        reason: Some(reason.to_string()),
        ..EdgeView::default()
    }
}

fn issued_state(leaf: &LeafInfo, issued_at: i64) -> AcmeState {
    let mut state = AcmeState::default();
    state.accounts.insert(
        DIRECTORY.to_string(),
        AccountRecord {
            account_url: account_url_for(DIRECTORY),
            vault_key: account_vault_key(DIRECTORY),
            recorded_at: issued_at,
        },
    );
    state.issued.push(IssuedRecord {
        serial: hex::encode(&leaf.serial),
        fingerprint: leaf.fingerprint.clone(),
        authority_key_id: leaf.authority_key_id.as_deref().map(hex::encode),
        not_before: leaf.not_before,
        not_after: leaf.not_after,
        name: NAME.to_string(),
        profile: None,
        directory_url: DIRECTORY.to_string(),
        account_url: account_url_for(DIRECTORY),
        issued_at,
        stored: true,
        installed: true,
        installed_at: Some(issued_at),
    });
    state
}

#[test]
fn the_decision_table_rows() {
    let pki = TestPki::new("Table");
    let now = chrono::Utc::now().timestamp();
    let settings = settings();
    let (chain, _) = pki.seeded(&[NAME], now - 10 * DAY, now + 80 * DAY);
    let leaf = leaf_of(&chain);
    let empty = AcmeState::default();

    // Another TLS mode, or a secret outside edge_tls*: left alone.
    let mut disk = view_serving(&leaf);
    disk.mode = Some("disk".to_string());
    assert!(matches!(
        decide(&disk, &settings, &empty, None, now),
        Decision::LeaveAlone(_)
    ));
    let mut other_key = view_serving(&leaf);
    other_key.tls_vault_key = Some("vault_db_password".to_string());
    assert!(matches!(
        decide(&other_key, &settings, &empty, None, now),
        Decision::LeaveAlone(_)
    ));

    // awaiting, not_found: order now, nothing to replace.
    assert_eq!(
        decide(&view_awaiting("not_found"), &settings, &empty, None, now),
        Decision::Order {
            replaces: None,
            reason: "the edge has no certificate: there is nothing to replace".to_string()
        }
    );
    // awaiting, any other verdict: reload, never order.
    for verdict in ["forbidden", "invalid_material", "transient"] {
        assert!(matches!(
            decide(&view_awaiting(verdict), &settings, &empty, None, now),
            Decision::Reload(_)
        ));
    }

    // A leaf this account issued: idle before its renewal point, ordered (with replaces) at it.
    let ours = issued_state(&leaf, now - 40 * DAY);
    let renew_at = now + 5 * DAY;
    assert!(matches!(
        decide(&view_serving(&leaf), &settings, &ours, Some(renew_at), now),
        Decision::Idle { renew_at: at, .. } if at == renew_at
    ));
    match decide(&view_serving(&leaf), &settings, &ours, Some(now - 1), now) {
        Decision::Order { replaces, .. } => {
            let replaces = replaces.expect("replaces");
            assert_eq!(replaces.serial, leaf.serial);
            assert_eq!(
                Some(replaces.authority_key_id),
                leaf.authority_key_id.clone()
            );
        }
        other => panic!("{other:?}"),
    }
    // Issued less than a third of its lifetime ago: the cap holds even a due renewal.
    let recent = issued_state(&leaf, now - 10 * DAY);
    assert!(matches!(
        decide(&view_serving(&leaf), &settings, &recent, Some(now - 1), now),
        Decision::Held { until: Some(until), .. } if until == now + 20 * DAY
    ));
    // Without ARI the renewal point is two thirds of the lifetime (day 60 of 90 here).
    let two_thirds = leaf.not_before + (leaf.not_after - leaf.not_before) * 2 / 3;
    assert!(matches!(
        decide(&view_serving(&leaf), &settings, &ours, None, two_thirds - 1),
        Decision::Idle { .. }
    ));
    assert!(matches!(
        decide(&view_serving(&leaf), &settings, &ours, None, two_thirds),
        Decision::Order { .. }
    ));

    // acme.json holds a newer certificate than the served one: reload, never order.
    let mut waiting = ours.clone();
    waiting.pending_install = Some(PendingInstall {
        fingerprint: "00".repeat(32),
        serial: "01".to_string(),
        vault_key: EDGE_KEY.to_string(),
        edge_node: EDGE.to_string(),
        written: Some(WriteIdentity {
            version: 2,
            updated_at: Some("2026-10-21T00:00:00Z".to_string()),
        }),
        stored_at: now,
    });
    assert!(matches!(
        decide(
            &view_serving(&leaf),
            &settings,
            &waiting,
            Some(now - 1),
            now
        ),
        Decision::Reload(_)
    ));
    assert!(matches!(
        decide(&view_awaiting("not_found"), &settings, &waiting, None, now),
        Decision::Reload(_)
    ));

    // A hand-seeded leaf, or one for another name: alerts only.
    assert!(matches!(
        decide(&view_serving(&leaf), &settings, &empty, None, now + 70 * DAY),
        Decision::AlertOnly(reason) if reason.contains("seeded by hand")
    ));
    let (other, _) = pki.seeded(&["other.example.org"], now - DAY, now + 80 * DAY);
    assert!(matches!(
        decide(&view_serving(&leaf_of(&other)), &settings, &empty, None, now),
        Decision::AlertOnly(reason) if reason.contains("does not cover")
    ));
    // A leaf this account issued for another name is not renewed automatically either.
    let mut renamed = settings.clone();
    renamed.name = "hive-2.example.com".to_string();
    assert!(matches!(
        decide(&view_serving(&leaf), &renamed, &ours, Some(now - 1), now),
        Decision::AlertOnly(_)
    ));
}

#[test]
fn a_wildcard_covers_the_name_and_is_judged_as_hand_seeded() {
    let pki = TestPki::new("Wildcard");
    let now = chrono::Utc::now().timestamp();
    let (chain, _) = pki.seeded(&["*.example.com"], now - DAY, now + 80 * DAY);
    let leaf = leaf_of(&chain);
    let mut state = AcmeState::default();
    state.edge.last_status = Some(view_serving(&leaf));
    let open = evaluate_conditions(Some(&settings()), &state, now);
    assert!(open.is_empty(), "{open:?}");
}

#[tokio::test]
async fn an_edge_that_does_not_answer_orders_nothing_and_alerts_after_an_hour() {
    for absent in [false, true] {
        let h = if absent {
            Harness::absent()
        } else {
            Harness::on()
        };
        h.mesh().edge_down = Some(
            "router returned UNREACHABLE for target=SY.edge@ingress1 reason=VPN_BLOCKED"
                .to_string(),
        );
        let now = h.now();
        assert_eq!(h.service.check_once().await, now + 600, "retried in 10 min");
        h.advance(30 * 60);
        h.service.check_once().await;
        assert!(h.alerts().is_empty());
        h.advance(31 * 60);
        h.service.check_once().await;
        assert_eq!(
            h.alerts(),
            vec![("edge_unreachable".to_string(), "open".to_string())]
        );
        h.advance(600);
        h.service.check_once().await;
        assert_eq!(h.alerts().len(), 1, "a standing condition writes nothing");
        {
            let ca = h.ca();
            assert!(ca.log.new_orders.is_empty() && ca.log.accounts_created.is_empty());
        }
        assert_eq!(h.state().backoff, Backoff::default(), "no CA backoff");
        assert!(h.state().edge.last_error.unwrap().contains("VPN_BLOCKED"));
        // It answers again: the condition closes.
        h.mesh().edge_down = None;
        let (chain, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
        h.mesh().seed(&chain, &key);
        h.service.check_once().await;
        assert!(h
            .alerts()
            .contains(&("edge_unreachable".to_string(), "closed".to_string())));
    }
}

#[tokio::test]
async fn expiry_is_judged_from_the_last_status_seen() {
    let h = Harness::absent();
    let now = h.now();
    // 90 days, 80 gone: less than a sixth (15 days) left.
    let (chain, key) = h.pki.seeded(&[NAME], now - 80 * DAY, now + 10 * DAY);
    h.mesh().seed(&chain, &key);
    h.service.check_once().await;
    assert_eq!(
        h.alerts(),
        vec![("expiring".to_string(), "open".to_string())]
    );
    assert_eq!(h.mesh().alerts[0]["severity"], "critical");
    // The edge stops answering: still expiring, judged from what it last said.
    h.mesh().edge_down = Some("timed out".to_string());
    h.advance(HOUR);
    h.service.check_once().await;
    assert!(h.open_conditions().contains(&"expiring".to_string()));
    assert!(!h
        .alerts()
        .contains(&("expiring".to_string(), "closed".to_string())));
}

#[tokio::test]
async fn without_acme_a_hand_seeded_certificate_gets_expiry_checks_only() {
    let h = Harness::absent();
    let now = h.now();
    let (chain, key) = h
        .pki
        .seeded(&["other.example.org"], now - 70 * DAY, now + 20 * DAY);
    h.mesh().seed(&chain, &key);
    h.service.check_once().await;
    assert!(h.open_conditions().is_empty(), "{:?}", h.open_conditions());
    assert_eq!(h.mesh().statuses, 1);
    assert!(h.ca().log.new_orders.is_empty());
    // An awaiting edge raises edge_without_certificate, acme or not.
    {
        let mut mesh = h.mesh();
        mesh.served = None;
        mesh.served_write = None;
    }
    h.service.check_once().await;
    assert_eq!(h.open_conditions(), vec!["edge_without_certificate"]);
}

#[tokio::test]
async fn with_acme_a_hand_seeded_certificate_is_alerted_at_its_renewal_point() {
    let h = Harness::on();
    let now = h.now();
    let (chain, key) = h
        .pki
        .seeded(&["*.example.com"], now - 50 * DAY, now + 40 * DAY);
    h.mesh().seed(&chain, &key);
    h.service.check_once().await;
    assert!(h.open_conditions().is_empty());
    h.advance(11 * DAY);
    h.service.check_once().await;
    assert_eq!(h.open_conditions(), vec!["foreign_certificate"]);
    assert!(h.ca().log.new_orders.is_empty(), "alert only");
    // One for another name raises name_not_covered at once.
    let (other, key) = h.pki.seeded(&["other.example.org"], now, now + 90 * DAY);
    h.mesh().seed(&other, &key);
    h.service.check_once().await;
    assert!(h
        .open_conditions()
        .contains(&"name_not_covered".to_string()));
    assert!(h.ca().log.new_orders.is_empty());
}

#[tokio::test]
async fn an_awaiting_edge_with_another_verdict_is_reloaded_then_alerted() {
    let h = Harness::on();
    h.mesh().awaiting_reason = "forbidden".to_string();
    // The vault holds a secret the edge may not read: a reload cannot fix it either.
    h.mesh().edge_down = None;
    h.service.check_once().await;
    assert_eq!(h.mesh().reloads, 1);
    assert!(h.ca().log.new_orders.is_empty());
    assert!(h
        .open_conditions()
        .contains(&"edge_without_certificate".to_string()));
}

#[tokio::test]
async fn ari_sets_a_random_point_of_the_window_and_is_asked_again_after_retry_after() {
    let h = Harness::on();
    h.install_first().await;
    let issued_at = h.now();
    let start = issued_at + 40 * DAY;
    let end = issued_at + 42 * DAY;
    h.ca().ari = Ok(AriAnswer {
        start,
        end,
        explanation_url: Some("https://ca.example.test/why".to_string()),
        retry_after_secs: Some(6 * HOUR),
    });
    h.advance(HOUR);
    let next = h.service.check_once().await;
    let plan = h.state().renewal.expect("a plan");
    assert_eq!(plan.source, "ari");
    assert_eq!(
        plan.renew_at,
        start + DAY,
        "half way: the random draw is 0.5"
    );
    assert_eq!(plan.next_ari_fetch_at, Some(h.now() + 6 * HOUR));
    assert_eq!(h.ca().log.ari_calls, 1);
    assert!(next <= h.now() + 6 * HOUR + 1_800);
    // Within Retry-After the window is not asked again.
    h.advance(HOUR);
    h.service.check_once().await;
    assert_eq!(h.ca().log.ari_calls, 1);
    // At the chosen point the renewal goes out, naming the leaf it replaces.
    h.clock.store(start + DAY, Ordering::SeqCst);
    h.service.check_once().await;
    let orders = h.ca().log.new_orders.clone();
    assert_eq!(orders.len(), 2);
    assert!(orders[1].2);
}

#[tokio::test]
async fn without_ari_the_renewal_is_at_two_thirds_of_the_lifetime() {
    let h = Harness::on();
    h.install_first().await;
    let leaf_not_before = h.state().issued[0].not_before;
    h.advance(DAY);
    h.service.check_once().await;
    let plan = h.state().renewal.expect("a plan");
    assert_eq!(plan.source, "lifetime");
    assert_eq!(plan.renew_at, leaf_not_before + 60 * DAY);
    h.clock
        .store(leaf_not_before + 60 * DAY - 10, Ordering::SeqCst);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 1);
    h.clock.store(leaf_not_before + 60 * DAY, Ordering::SeqCst);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 2);
}

#[tokio::test]
async fn a_served_leaf_48_hours_past_its_renewal_point_is_overdue() {
    let h = Harness::on();
    h.install_first().await;
    let not_before = h.state().issued[0].not_before;
    // The CA refuses every renewal.
    h.ca().poll = Ok(OrderStatus::Invalid);
    h.clock
        .store(not_before + 60 * DAY + 49 * HOUR, Ordering::SeqCst);
    h.service.check_once().await;
    let open = h.open_conditions();
    assert!(open.contains(&"renewal_overdue".to_string()), "{open:?}");
    assert!(open.contains(&"renewal_failed".to_string()));
    // A sixth of 90 days is 15: at day 75.5 it is also expiring.
    h.clock.store(not_before + 76 * DAY, Ordering::SeqCst);
    h.service.check_once().await;
    assert!(h.open_conditions().contains(&"expiring".to_string()));
}

// ---------------------------------------------------------------------------------------------
// The backoff, the rate limit and the cap (§7)
// ---------------------------------------------------------------------------------------------

#[test]
fn the_backoff_doubles_from_an_hour_to_a_day() {
    let delays: Vec<i64> = (1..=7).map(backoff_delay).collect();
    assert_eq!(
        delays,
        vec![HOUR, 2 * HOUR, 4 * HOUR, 8 * HOUR, 16 * HOUR, DAY, DAY]
    );
    assert_eq!(backoff_delay(40), DAY);
}

#[test]
fn a_rate_limit_waits_for_the_time_the_ca_sends_or_a_day() {
    let now = 1_767_225_600; // 2026-01-01T00:00:00Z
    let detail = "API error: too many certificates (5) already issued for this exact set of identifiers in the last 168h0m0s, retry after 2026-01-02 03:04:05 UTC: see https://letsencrypt.org/docs/rate-limits/ (urn:ietf:params:acme:error:rateLimited)";
    assert_eq!(rate_limited_until(detail, now), now + DAY + 3 * HOUR + 245);
    assert_eq!(rate_limited_until("rate limited", now), now + DAY);
    // A time in the past, or absurdly far, is not trusted.
    assert_eq!(
        rate_limited_until("retry after 2025-01-01 00:00:00 UTC", now),
        now + DAY
    );
    assert_eq!(
        rate_limited_until("retry after 2027-01-01 00:00:00 UTC", now),
        now + DAY
    );
}

#[tokio::test]
async fn a_rate_limited_order_holds_every_order_until_the_time_the_ca_sends() {
    let h = Harness::on();
    let now = h.now();
    let until = chrono::DateTime::from_timestamp(now + 5 * HOUR, 0).unwrap();
    h.ca()
        .new_order_errors
        .push_back(CaError::RateLimited(format!(
            "too many new orders, retry after {} UTC",
            until.format("%Y-%m-%d %H:%M:%S")
        )));
    let next = h.service.check_once().await;
    let state = h.state();
    assert_eq!(state.backoff.rate_limited_until, Some(now + 5 * HOUR));
    assert_eq!(state.backoff.failures, 0);
    assert_eq!(next, now + 5 * HOUR);
    assert_eq!(
        h.service.begin_renew().unwrap_err().error_code,
        "ACME_RATE_LIMITED"
    );
    h.advance(HOUR);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 1);
    // A restart keeps honouring it: it is in acme.json.
    let mut h = h;
    h.restart();
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 1);
    h.advance(4 * HOUR);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 2);
}

#[tokio::test]
async fn the_backoff_survives_a_restart() {
    let mut h = Harness::on();
    h.ca().poll = Ok(OrderStatus::Invalid);
    h.service.check_once().await;
    h.ca().poll = Ok(OrderStatus::Ready);
    h.restart();
    h.advance(30 * 60);
    h.service.check_once().await;
    assert_eq!(
        h.ca().log.new_orders.len(),
        1,
        "a restart never spends an order"
    );
    h.advance(31 * 60);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 2);
}

#[tokio::test]
async fn the_cap_allows_one_issuance_per_third_of_the_lifetime_unless_forced() {
    let h = Harness::on();
    h.install_first().await;
    // The secret disappears: an awaiting edge would be ordered for, but not within the cap.
    {
        let mut mesh = h.mesh();
        mesh.vault.remove(EDGE_KEY);
        mesh.serve_current();
    }
    h.advance(DAY);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 1);
    assert!(h.state().last_check.unwrap().decision.contains("the cap"));
    // Forced, it orders.
    h.service.begin_renew().expect("renew");
    h.settle().await;
    assert_eq!(h.ca().log.new_orders.len(), 2);
    assert_eq!(h.state().last_attempt.unwrap().result, "installed");
    // Past a third of the lifetime, the check orders again on its own.
    {
        let mut mesh = h.mesh();
        mesh.vault.remove(EDGE_KEY);
        mesh.serve_current();
    }
    h.advance(31 * DAY);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 3);
}

// ---------------------------------------------------------------------------------------------
// Conditions and their alerts (§7)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_alert_the_orchestrator_did_not_take_is_written_once_later() {
    let h = Harness::absent();
    h.mesh().awaiting_reason = "not_found".to_string();
    h.mesh().alerts_down = true;
    h.service.check_once().await;
    assert!(h.alerts().is_empty());
    assert!(!h.state().conditions["edge_without_certificate"].alerted);
    h.mesh().alerts_down = false;
    h.service.check_once().await;
    h.service.check_once().await;
    assert_eq!(
        h.alerts(),
        vec![("edge_without_certificate".to_string(), "open".to_string())]
    );
    // A condition that closes before its open row was written writes nothing at all.
    let h = Harness::absent();
    h.mesh().alerts_down = true;
    h.service.check_once().await;
    let now = h.now();
    let (chain, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&chain, &key);
    h.mesh().alerts_down = false;
    h.service.check_once().await;
    assert!(h.alerts().is_empty());
    assert!(h.state().unsent_closures.is_empty());
}

#[test]
fn every_condition_kind_is_one_the_orchestrator_takes() {
    let pki = TestPki::new("Kinds");
    let now = chrono::Utc::now().timestamp();
    let (chain, _) = pki.seeded(&["other.example.org"], now - 80 * DAY, now + 10 * DAY);
    let leaf = leaf_of(&chain);
    let mut state = issued_state(&leaf, now - 80 * DAY);
    state.issued[0].name = "other.example.org".to_string();
    state.edge.node = Some(EDGE.to_string());
    state.edge.last_status = Some(view_awaiting("forbidden"));
    state.edge.unreachable_since_at = Some(now - 2 * HOUR);
    state.last_attempt = Some(AttemptRecord {
        kind: RunKind::Check,
        directory_url: DIRECTORY.to_string(),
        started_at: now,
        finished_at: now,
        result: "failed".to_string(),
        stage: Some("challenge".to_string()),
        detail: Some("invalid".to_string()),
        fingerprint: None,
        trusted: None,
    });
    state.install_refused = Some(InstallRefused {
        fingerprint: "aa".repeat(32),
        serial: "01".to_string(),
        detail: "expired".to_string(),
        rolled_back: true,
        refused_at: now,
    });
    let mut kinds: Vec<String> = evaluate_conditions(Some(&settings()), &state, now)
        .into_keys()
        .collect();
    state.edge.last_status = Some(view_serving(&leaf));
    kinds.extend(evaluate_conditions(Some(&settings()), &state, now).into_keys());
    state.issued.clear();
    kinds.extend(evaluate_conditions(Some(&settings()), &state, now + 70 * DAY).into_keys());
    // renewal_overdue: ours, covered, 48 h past its point.
    let (ours_chain, _) = pki.seeded(&[NAME], now - 80 * DAY, now + 10 * DAY);
    let ours_leaf = leaf_of(&ours_chain);
    let mut ours = issued_state(&ours_leaf, now - 80 * DAY);
    ours.edge.last_status = Some(view_serving(&ours_leaf));
    kinds.extend(evaluate_conditions(Some(&settings()), &ours, now).into_keys());
    kinds.sort();
    kinds.dedup();
    assert_eq!(
        kinds,
        vec![
            "edge_unreachable",
            "edge_without_certificate",
            "expiring",
            "foreign_certificate",
            "install_refused",
            "name_not_covered",
            "renewal_failed",
            "renewal_overdue",
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// The edge's secret and the account (§6 step 1 and §6 "The account")
// ---------------------------------------------------------------------------------------------

#[test]
fn only_edge_tls_secrets_are_written() {
    for ok in ["edge_tls", "edge_tls_fluxbee_ai", "edge_tls:ingress1"] {
        assert!(check_edge_secret_name(ok).is_ok(), "{ok}");
    }
    for refused in ["vault_db", "acme_account:0011", "EDGE_TLS", "tls_edge", ""] {
        assert!(check_edge_secret_name(refused).is_err(), "{refused}");
    }
}

#[test]
fn the_edge_secret_metadata_must_be_the_edges() {
    let good = SecretMetadata {
        resource_type: Some("tls".to_string()),
        tenant_id: Some(DEFAULT_ROOT_TENANT_ID.to_string()),
        owner_ilk: Some(edge_ilk()),
        version: Some(3),
        updated_at: Some("2026-10-21T00:00:00Z".to_string()),
        created_by: Some(admin_ilk()),
    };
    assert!(check_edge_secret_metadata(&good, EDGE).is_ok());
    let mut bad = good.clone();
    bad.resource_type = Some("bearer_token".to_string());
    assert!(check_edge_secret_metadata(&bad, EDGE).is_err());
    let mut bad = good.clone();
    bad.tenant_id = Some("tnt:43d576a3-d712-4d91-9245-5d5463dd693e".to_string());
    assert!(check_edge_secret_metadata(&bad, EDGE).is_err());
    let mut bad = good.clone();
    bad.owner_ilk = Some(fluxbee_sdk::deterministic_system_ilk_id("SY.edge@ingress2"));
    assert!(check_edge_secret_metadata(&bad, EDGE).is_err());
    let mut bad = good;
    bad.owner_ilk = None;
    assert!(check_edge_secret_metadata(&bad, EDGE).is_err());
}

#[tokio::test]
async fn a_secret_outside_edge_tls_or_not_the_edges_is_never_written_and_no_ca_is_asked() {
    // Another name: the check leaves it alone, a forced renewal fails at the look.
    let h = Harness::on();
    h.mesh().edge_key = "vault_db_password".to_string();
    h.service.check_once().await;
    assert!(h
        .state()
        .last_check
        .unwrap()
        .decision
        .starts_with("leave alone"));
    h.service.begin_renew().expect("renew");
    h.settle().await;
    let attempt = h.state().last_attempt.unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("look"));
    assert!(attempt.detail.unwrap().contains("edge_tls"));

    // An edge_tls secret owned by someone else.
    let h = Harness::on();
    let now = h.now();
    let (chain, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&chain, &key);
    h.mesh().vault.get_mut(EDGE_KEY).unwrap().metadata["ilk"] = json!(
        fluxbee_sdk::deterministic_system_ilk_id("SY.architect@motherbee")
    );
    h.service.begin_renew().expect("renew");
    h.settle().await;
    let attempt = h.state().last_attempt.unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("look"));
    assert!(attempt.detail.unwrap().contains("not owned by"));

    // A vault that does not answer: nothing is asked of the CA, and no CA backoff.
    let h = Harness::on();
    h.mesh().vault_down = true;
    h.service.check_once().await;
    let state = h.state();
    let attempt = state.last_attempt.unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("look"));
    assert_eq!(state.backoff, Backoff::default());
    let ca = h.ca();
    assert!(ca.log.new_orders.is_empty() && ca.log.accounts_created.is_empty());
}

#[tokio::test]
async fn an_account_whose_url_differs_from_acme_json_is_refused() {
    let h = Harness::on();
    h.mesh().put(
        &account_vault_key(DIRECTORY),
        json!({ "id": "https://ca.example.test/acct/swapped" }),
        json!({}),
    );
    let now = h.now();
    h.service.update(|state| {
        state.accounts.insert(
            DIRECTORY.to_string(),
            AccountRecord {
                account_url: account_url_for(DIRECTORY),
                vault_key: account_vault_key(DIRECTORY),
                recorded_at: now,
            },
        )
    });
    h.service.check_once().await;
    let attempt = h.state().last_attempt.unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("account"));
    assert!(attempt.detail.unwrap().contains("refused"));
    assert!(h.ca().log.new_orders.is_empty());
    assert_eq!(
        h.state().accounts[DIRECTORY].account_url,
        account_url_for(DIRECTORY),
        "the record stands"
    );
}

#[tokio::test]
async fn an_existing_account_is_reused_and_recorded_when_acme_json_has_none() {
    let h = Harness::on();
    h.mesh().put(
        &account_vault_key(DIRECTORY),
        json!({ "id": account_url_for(DIRECTORY) }),
        json!({}),
    );
    h.install_first().await;
    assert!(h.ca().log.accounts_created.is_empty());
    assert_eq!(
        h.state().accounts[DIRECTORY].account_url,
        account_url_for(DIRECTORY)
    );
}

// ---------------------------------------------------------------------------------------------
// The test order (D5)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_test_order_uses_staging_stores_nothing_and_reports_the_untrusted_root() {
    let mut h = Harness::on();
    // The platform does not trust the staging CA.
    h.platform = Arc::new(TestPki::new("Platform"));
    h.restart();
    h.service.begin_test().expect("test starts");
    h.settle().await;
    let state = h.state();
    let test = state.last_test.clone().expect("the test result");
    assert_eq!(test.result, "tested", "{test:?}");
    assert_eq!(test.trusted, Some(false));
    assert!(test.detail.unwrap().contains("not trusted"));
    assert_eq!(test.directory_url, STAGING);
    assert!(state.last_attempt.is_none() && state.issued.is_empty());
    assert!(state.order.is_none() && state.pending_install.is_none());
    assert_eq!(state.backoff, Backoff::default());
    assert_eq!(
        state.accounts[STAGING].account_url,
        account_url_for(STAGING)
    );
    let mesh = h.mesh();
    assert!(!mesh.vault.contains_key(EDGE_KEY), "nothing stored");
    assert_eq!(mesh.reloads, 0, "nothing installed");
    assert_eq!(mesh.arms.len(), 1);
    assert_eq!(mesh.clears.len(), 1);
    let account = mesh
        .vault
        .get(&account_vault_key(STAGING))
        .expect("staging account");
    assert_eq!(account.metadata["description"], STAGING);
    drop(mesh);
    assert_eq!(h.ca().log.accounts_created, vec![STAGING.to_string()]);
    assert!(!h.ca().log.new_orders[0].2, "never replaces");
}

#[tokio::test]
async fn a_test_order_is_refused_when_staging_is_production() {
    let mut same = settings();
    same.staging_directory_url = same.directory_url.clone();
    let h = Harness::new(AcmeConfig::On(same));
    let refusal = h.service.begin_test().unwrap_err();
    assert_eq!(refusal.error_code, "ACME_TEST_SAME_DIRECTORY");
    assert_eq!(refusal.http_status, 409);
}

#[tokio::test]
async fn without_acme_renew_and_test_are_refused_and_reload_works() {
    let h = Harness::absent();
    assert_eq!(h.service.begin_renew().unwrap_err().error_code, "ACME_OFF");
    assert_eq!(h.service.begin_test().unwrap_err().error_code, "ACME_OFF");
    let now = h.now();
    let (chain, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&chain, &key);
    assert!(h.service.reload_action().await.is_ok());
}

// ---------------------------------------------------------------------------------------------
// The configuration (§3)
// ---------------------------------------------------------------------------------------------

fn yaml(text: &str) -> serde_yaml::Value {
    serde_yaml::from_str(text).expect("yaml")
}

fn off_reason(config: &AcmeConfig) -> String {
    match config {
        AcmeConfig::Off { reason, .. } => reason.clone(),
        AcmeConfig::On(_) => panic!("expected off, got on"),
        AcmeConfig::Absent { .. } => panic!("expected off, got absent"),
    }
}

#[test]
fn the_name_is_the_lowercase_host_of_the_public_base_url() {
    for (url, name) in [
        ("https://Hive-K3M9.Example.com", "hive-k3m9.example.com"),
        ("https://hive.example.com.", "hive.example.com"),
        ("https://hive.example.com:443/base/", "hive.example.com"),
        ("HTTPS://hive.example.com?x=1", "hive.example.com"),
    ] {
        assert_eq!(certified_name(Some(url)).as_deref(), Ok(name), "{url}");
    }
    for (url, why) in [
        (None, "missing"),
        (Some("  "), "missing"),
        (Some("http://hive.example.com"), "not an https URL"),
        (Some("https://hive.example.com:8443"), "port 8443"),
        (Some("https://203.0.113.7"), "IP literal"),
        (Some("https://[2001:db8::1]"), "IP literal"),
        (Some("https://*.example.com"), "wildcard"),
        (Some("https://localhost"), "no dot"),
        (Some("https://user@hive.example.com"), "user information"),
        (Some("https://hive_1.example.com"), "not a DNS name"),
    ] {
        let err = certified_name(url).unwrap_err();
        assert!(err.contains(why), "{url:?}: {err}");
    }
}

#[test]
fn the_acme_section_is_on_off_or_absent_and_never_fails_the_boot() {
    let url = Some("https://hive-1.example.com");
    assert!(matches!(
        AcmeConfig::from_hive(None, url),
        AcmeConfig::Absent { name: Some(ref name) } if name == NAME
    ));
    match AcmeConfig::from_hive(Some(&yaml("terms_agreed: true")), url) {
        AcmeConfig::On(settings) => {
            assert_eq!(settings.name, NAME);
            assert_eq!(settings.directory_url, LETS_ENCRYPT_DIRECTORY);
            assert_eq!(
                settings.staging_directory_url,
                LETS_ENCRYPT_STAGING_DIRECTORY
            );
            assert_eq!(settings.profile, None);
        }
        _ => panic!("expected on"),
    }
    match AcmeConfig::from_hive(
        Some(&yaml(
            "terms_agreed: true\ndirectory_url: https://acme.example.net/dir\nstaging_directory_url: https://acme-staging.example.net/dir\nprofile: classic\ncontact: mailto:ops@example.com",
        )),
        url,
    ) {
        AcmeConfig::On(settings) => {
            assert_eq!(settings.directory_url, "https://acme.example.net/dir");
            assert_eq!(settings.profile.as_deref(), Some("classic"));
            assert_eq!(settings.contact.as_deref(), Some("mailto:ops@example.com"));
        }
        _ => panic!("expected on"),
    }
    for (section, why) in [
        ("{}", "terms_agreed"),
        ("terms_agreed: false", "terms_agreed"),
        ("terms_agreed: \"yes\"", "terms_agreed"),
        ("terms_agreed: true\nprofile: shortlived", "shortlived"),
        (
            "terms_agreed: true\nprofle: classic",
            "acme.profle is not a setting",
        ),
        (
            "terms_agreed: true\ndirectory_url: http://acme.example.net/dir",
            "https",
        ),
        (
            "terms_agreed: true\ntrust_anchor: /nonexistent/acme-ca.pem",
            "cannot be read",
        ),
        ("[1, 2]", "not a mapping"),
    ] {
        let reason = off_reason(&AcmeConfig::from_hive(Some(&yaml(section)), url));
        assert!(reason.contains(why), "{section}: {reason}");
    }
    for (base, why) in [
        (None, "missing"),
        (Some("https://hive.example.com:8443"), "port"),
        (Some("https://198.51.100.1"), "IP literal"),
        (Some("https://*.example.com"), "wildcard"),
        (Some("https://hive"), "no dot"),
    ] {
        let reason = off_reason(&AcmeConfig::from_hive(
            Some(&yaml("terms_agreed: true")),
            base,
        ));
        assert!(reason.contains(why), "{base:?}: {reason}");
    }
}

#[test]
fn a_trust_anchor_is_read_at_boot() {
    let pki = TestPki::new("Anchor");
    let dir = temp_dir("anchor");
    let path = dir.join("acme-ca.pem");
    fs::write(&path, &pki.root_pem).expect("write");
    let section = yaml(&format!(
        "terms_agreed: true\ntrust_anchor: {}",
        path.display()
    ));
    match AcmeConfig::from_hive(Some(&section), Some("https://hive-1.example.com")) {
        AcmeConfig::On(settings) => assert_eq!(settings.trust_anchor_roots.len(), 1),
        _ => panic!("expected on"),
    }
    let empty = dir.join("empty.pem");
    fs::write(&empty, "not a certificate").expect("write");
    let section = yaml(&format!(
        "terms_agreed: true\ntrust_anchor: {}",
        empty.display()
    ));
    assert!(off_reason(&AcmeConfig::from_hive(
        Some(&section),
        Some("https://hive-1.example.com")
    ))
    .contains("holds no certificate"));
}

#[tokio::test]
async fn a_profile_the_directory_does_not_offer_turns_acme_off_at_the_first_check() {
    let mut chosen = settings();
    chosen.profile = Some("tlsserver-2027".to_string());
    let h = Harness::new(AcmeConfig::On(chosen));
    h.service.check_once().await;
    assert!(h.ca().log.new_orders.is_empty());
    let status = h.service.status().await;
    assert_eq!(status["config"]["state"], "off");
    assert!(status["config"]["reason"]
        .as_str()
        .unwrap()
        .contains("not offered"));
    assert_eq!(h.service.begin_renew().unwrap_err().error_code, "ACME_OFF");
}

#[tokio::test]
async fn an_unreadable_acme_json_is_set_aside_and_acme_starts_empty() {
    let mut h = Harness::on();
    fs::create_dir_all(h.state_path().parent().unwrap()).unwrap();
    fs::write(h.state_path(), "{ not json").unwrap();
    h.restart();
    assert_eq!(h.state(), AcmeState::default());
    let status = h.service.status().await;
    assert!(status["warnings"][0]
        .as_str()
        .unwrap()
        .contains("does not parse"));
}

#[test]
fn account_keys_are_reserved_and_one_per_directory() {
    assert!(is_acme_account_key("acme_account:0123456789abcdef"));
    assert!(is_acme_account_key(" ACME_ACCOUNT:0123456789abcdef "));
    assert!(is_acme_account_key("acme_account"));
    // The same namespace Cloud may not write.
    assert!(fluxbee_sdk::vault::is_cloud_reserved_vault_key(
        &account_vault_key(DIRECTORY)
    ));
    assert!(!is_acme_account_key("edge_tls"));
    assert!(!is_acme_account_key("acme:stripe:key"));
    assert_ne!(account_vault_key(DIRECTORY), account_vault_key(STAGING));
    assert!(fluxbee_sdk::vault::vault_key_is_valid(&account_vault_key(
        LETS_ENCRYPT_DIRECTORY
    )));
}

// ---------------------------------------------------------------------------------------------
// The status (§7)
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_status_shows_the_configuration_the_edge_the_certificates_and_the_conditions() {
    let h = Harness::on();
    h.install_first().await;
    let status = h.service.status().await;
    assert_eq!(status["config"]["state"], "on");
    assert_eq!(status["config"]["name"], NAME);
    assert_eq!(status["terms_url"], "https://ca.example.test/terms");
    assert_eq!(
        status["accounts"][0]["account_url"],
        account_url_for(DIRECTORY)
    );
    assert_eq!(status["edge"]["node"], EDGE);
    assert_eq!(status["edge"]["live"]["state"], "serving");
    assert_eq!(status["edge"]["name_covered"], true);
    assert_eq!(status["edge"]["issued_by_this_account"], true);
    assert_eq!(status["last_attempt"]["result"], "installed");
    assert!(status["in_flight"].is_null());
    assert!(status["conditions"].as_array().unwrap().is_empty());
    // Times read as RFC 3339.
    assert!(status["next_check_at"].as_str().unwrap().ends_with('Z'));
    assert!(status["issued"][0]["not_after"]
        .as_str()
        .unwrap()
        .ends_with('Z'));
    let text = status.to_string();
    assert!(!text.contains("PRIVATE KEY") && !text.contains("key_pkcs8"));
}

#[test]
fn an_edge_answer_reads_the_leaf_from_its_pem_or_its_fields() {
    let pki = TestPki::new("Answer");
    let now = chrono::Utc::now().timestamp();
    let (chain, _) = pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    let leaf = leaf_of(&chain);
    let from_pem = EdgeView::from_answer(&json!({
        "status": "ok",
        "mode": "vault",
        "tls_vault_key": "edge_tls",
        "state": "serving",
        "leaf_pem": first_pem(&chain),
        "chain_len": 2,
    }))
    .unwrap();
    let from_fields = EdgeView::from_answer(&json!({
        "status": "ok",
        "mode": "vault",
        "tls_vault_key": "edge_tls",
        "state": "serving",
        "fingerprint": leaf.fingerprint.to_uppercase(),
        "not_before": leaf.not_before,
        "not_after": leaf.not_after,
        "names": [NAME.to_uppercase()],
        "chain_len": 2,
    }))
    .unwrap();
    for view in [&from_pem, &from_fields] {
        let read = view.leaf.as_ref().unwrap();
        assert_eq!(read.fingerprint, leaf.fingerprint);
        assert_eq!(read.not_after, leaf.not_after);
        assert_eq!(read.names, vec![NAME.to_string()]);
        assert_eq!(read.chain_len, Some(2));
    }
    let refused = EdgeView::from_answer(&json!({
        "status": "ok",
        "mode": "vault",
        "state": "serving",
        "changed": false,
        "refused": { "reason": "invalid_material", "detail": "the chain is truncated" },
    }))
    .unwrap();
    let refusal = refused.refused.expect("refused");
    assert_eq!(refusal.reason, "invalid_material");
    assert_eq!(refusal.detail, "the chain is truncated");
    assert!(!refusal.transient());
    assert_eq!(refused.changed, Some(false));
    let awaiting = EdgeView::from_answer(&json!({
        "status": "ok",
        "mode": "vault",
        "tls_vault_key": "edge_tls",
        "state": "awaiting",
        "reason": "forbidden",
        "detail": "vault get 'edge_tls' failed: UNAUTHORIZED",
        "leaf_pem": null,
        "fingerprint": null,
    }))
    .unwrap();
    assert_eq!(awaiting.reason.as_deref(), Some("forbidden"));
    assert!(awaiting.detail.unwrap().contains("UNAUTHORIZED"));
    assert!(awaiting.leaf.is_none());
    let plaintext = EdgeView::from_answer(&json!({
        "status": "ok",
        "mode": "plaintext",
        "tls_vault_key": null,
        "state": "serving",
        "leaf_pem": null,
    }))
    .unwrap();
    assert!(plaintext.leaf.is_none() && plaintext.tls_vault_key.is_none());
    assert!(
        EdgeView::from_answer(&json!({"status": "error", "error": "not a vault edge"})).is_err()
    );
}

// ---------------------------------------------------------------------------------------------
// Writes: only onto what the edge serves, and rollbacks only of SY.admin's own write
// ---------------------------------------------------------------------------------------------

/// A refusal whose rollback SY.vault applied but whose answer was lost: the retry finds the
/// refused write no longer current and does not roll back again (which would swap it back).
#[tokio::test]
async fn a_rollback_whose_answer_was_lost_is_not_applied_twice() {
    let h = Harness::on();
    let now = h.now();
    let (good, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&good, &key);
    h.mesh().reload_refuses = Some("key mismatch".to_string());
    h.mesh().rollback_unanswered = true;
    h.service.begin_renew().expect("renew");
    h.settle().await;
    assert!(h.state().rollback_pending.is_some());
    assert_eq!(h.mesh().secret_cert().as_deref(), Some(good.as_str()));
    h.mesh().rollback_unanswered = false;
    h.service.check_once().await;
    let state = h.state();
    assert!(state.rollback_pending.is_none());
    assert!(state.install_refused.unwrap().rolled_back);
    assert_eq!(h.mesh().rollbacks, 1, "never a second rollback");
    assert_eq!(h.mesh().secret_cert().as_deref(), Some(good.as_str()));
}

/// The operator rolled back by hand before the retry: the retry leaves it.
#[tokio::test]
async fn a_manual_rollback_before_the_retry_is_not_undone() {
    let h = Harness::on();
    let now = h.now();
    let (good, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&good, &key);
    h.mesh().reload_refuses = Some("key mismatch".to_string());
    h.mesh().rollback_down = true;
    h.service.begin_renew().expect("renew");
    h.settle().await;
    assert!(h.state().rollback_pending.is_some());
    assert_ne!(h.mesh().secret_cert().as_deref(), Some(good.as_str()));
    // By hand: POST /hives/motherbee/vault/secrets/edge_tls/rollback.
    h.mesh().rollback(EDGE_KEY).expect("rolled back");
    h.mesh().rollback_down = false;
    h.service.check_once().await;
    assert!(h.state().rollback_pending.is_none());
    assert_eq!(h.mesh().rollbacks, 0, "SY.admin rolled nothing back");
    assert_eq!(h.mesh().secret_cert().as_deref(), Some(good.as_str()));
}

/// The vault holds a write the edge does not serve (the operator put one the edge refuses):
/// SY.admin reloads the edge once, then writes nothing over it, and asks nothing of the CA.
#[tokio::test]
async fn a_vault_version_the_edge_does_not_serve_is_never_written_over() {
    let h = Harness::on();
    let now = h.now();
    let (served, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&served, &key);
    let (other, other_key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().put(
        EDGE_KEY,
        json!({ "cert": other, "key": other_key }),
        MeshScript::edge_metadata(),
    );
    h.mesh().reload_refuses = Some("key mismatch".to_string());
    h.service.begin_renew().expect("renew");
    h.settle().await;
    let attempt = h.state().last_attempt.unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("look"));
    assert!(attempt
        .detail
        .unwrap()
        .contains("the vault holds a version the edge does not serve"));
    assert_eq!(
        h.mesh().reloads,
        1,
        "one reload, to re-align a metadata-only change"
    );
    {
        let ca = h.ca();
        assert!(ca.log.new_orders.is_empty() && ca.log.accounts_created.is_empty());
    }
    assert_eq!(h.mesh().secret_cert().as_deref(), Some(other.as_str()));
    h.service.check_once().await;
    assert!(h.open_conditions().contains(&"renewal_failed".to_string()));
}

/// A write the edge had not loaded yet, and takes at the reload: the edge now serves another
/// certificate than the look saw, so nothing is written, and the next check decides again.
#[tokio::test]
async fn a_certificate_the_edge_takes_at_the_reload_is_not_written_over() {
    let h = Harness::on();
    let now = h.now();
    let (served, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&served, &key);
    let (other, other_key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().put(
        EDGE_KEY,
        json!({ "cert": other, "key": other_key }),
        MeshScript::edge_metadata(),
    );
    h.service.begin_renew().expect("renew");
    h.settle().await;
    let attempt = h.state().last_attempt.unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("look"));
    assert!(attempt.detail.unwrap().contains("changed since the look"));
    assert!(h.ca().log.new_orders.is_empty());
    assert_eq!(h.mesh().served.as_deref(), Some(other.as_str()));
}

/// A metadata-only write (VAULT_ASSIGN) moves `updated_at` without changing the certificate: one
/// reload re-aligns the edge, and the order goes on.
#[tokio::test]
async fn a_metadata_only_change_is_realigned_by_one_reload() {
    let h = Harness::on();
    let now = h.now();
    let (served, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&served, &key);
    {
        let mut mesh = h.mesh();
        let at = mesh.tick();
        mesh.vault.get_mut(EDGE_KEY).unwrap().current.updated_at = at;
    }
    h.service.begin_renew().expect("renew");
    h.settle().await;
    let state = h.state();
    assert_eq!(state.last_attempt.unwrap().result, "installed");
    let mesh = h.mesh();
    let secret = &mesh.vault[EDGE_KEY];
    assert_eq!(
        secret.previous.as_ref().unwrap().value["cert"].as_str(),
        Some(served.as_str()),
        "the served certificate stays as the previous version"
    );
}

/// An edge with no certificate (awaiting, not_found) whose secret appeared since: not written.
#[tokio::test]
async fn an_edge_with_no_certificate_whose_secret_appeared_is_not_written_over() {
    let h = Harness::on();
    let now = h.now();
    let (chain, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().put(
        EDGE_KEY,
        json!({ "cert": chain, "key": key }),
        MeshScript::edge_metadata(),
    );
    h.service.check_once().await;
    let attempt = h.state().last_attempt.unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("look"));
    assert!(h.ca().log.new_orders.is_empty());
    assert_eq!(h.mesh().secret_cert().as_deref(), Some(chain.as_str()));
}

/// The secret changes between the look and the put: the issued certificate is dropped, not
/// written over the new write, and it does not count for the cap.
#[tokio::test]
async fn a_secret_that_changes_during_the_order_is_not_written_over() {
    let h = Harness::on();
    let now = h.now();
    let (foreign, foreign_key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    let mesh = Arc::clone(&h.mesh);
    let written = foreign.clone();
    h.ca().on_certificate = Some(Box::new(move || {
        lock(&mesh).put(
            EDGE_KEY,
            json!({ "cert": written, "key": foreign_key }),
            MeshScript::edge_metadata(),
        );
    }));
    h.service.check_once().await;
    let state = h.state();
    let attempt = state.last_attempt.clone().unwrap();
    assert_eq!(attempt.stage.as_deref(), Some("store"));
    assert!(attempt.detail.unwrap().contains("changed since the look"));
    assert!(!state.issued[0].stored);
    assert!(state.pending_install.is_none());
    let mesh = h.mesh();
    let secret = mesh.vault.get(EDGE_KEY).unwrap();
    assert_eq!(
        secret.current.value["cert"].as_str(),
        Some(foreign.as_str())
    );
    assert_eq!(secret.current.number, 1);
    assert!(
        secret.previous.is_none(),
        "nothing of SY.admin's was written"
    );
}

/// A put SY.vault applied but whose answer was lost is SY.admin's own write: kept, not dropped.
#[tokio::test]
async fn a_put_whose_answer_was_lost_is_recognized_as_ours() {
    let h = Harness::on();
    h.mesh().puts_unanswered.insert(EDGE_KEY.to_string(), 1);
    h.service.check_once().await;
    let state = h.state();
    assert_eq!(
        state.last_attempt.as_ref().unwrap().result,
        "installed",
        "{:?}",
        state.last_attempt
    );
    let mesh = h.mesh();
    let secret = mesh.vault.get(EDGE_KEY).unwrap();
    assert_eq!(secret.current.number, 1, "one put only");
    assert!(secret.previous.is_none());
}

/// A pending install is SY.admin's write (version and updated_at): the same number back after
/// two rollbacks is another write.
#[tokio::test]
async fn a_pending_install_is_matched_by_its_write_not_its_number() {
    let h = Harness::on();
    let now = h.now();
    let (good, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&good, &key);
    h.mesh().reload_down = true;
    h.service.begin_renew().expect("renew");
    h.settle().await;
    let pending = h.state().pending_install.expect("stored, not served");
    let written = pending.written.clone().expect("the write read back");
    assert_eq!(written.version, 2);
    {
        let mut mesh = h.mesh();
        mesh.rollback(EDGE_KEY).expect("first");
        mesh.rollback(EDGE_KEY).expect("second");
        assert_eq!(mesh.vault[EDGE_KEY].current.number, 2);
    }
    h.mesh().reload_down = false;
    h.service.check_once().await;
    assert!(
        h.state().pending_install.is_none(),
        "another write: no longer waited for"
    );
}

#[test]
fn the_cap_counts_only_stored_certificates() {
    let pki = TestPki::new("Cap");
    let now = chrono::Utc::now().timestamp();
    let (chain, _) = pki.seeded(&[NAME], now - DAY, now + 89 * DAY);
    let leaf = leaf_of(&chain);
    let mut state = issued_state(&leaf, now - DAY);
    assert_eq!(
        cap_until(&state, DIRECTORY, now),
        Some(now - DAY + 30 * DAY)
    );
    // Stored, then refused and rolled back: neither served nor waiting.
    state.issued[0].installed = false;
    assert_eq!(cap_until(&state, DIRECTORY, now), None);
    // Stored and waiting to be served.
    state.pending_install = Some(PendingInstall {
        fingerprint: leaf.fingerprint.clone(),
        serial: "01".to_string(),
        vault_key: EDGE_KEY.to_string(),
        edge_node: EDGE.to_string(),
        written: None,
        stored_at: now,
    });
    assert!(cap_until(&state, DIRECTORY, now).is_some());
    // Never stored.
    state.pending_install = None;
    state.issued[0].stored = false;
    assert_eq!(cap_until(&state, DIRECTORY, now), None);
}

#[tokio::test]
async fn an_issuance_never_stored_does_not_hold_an_edge_with_no_certificate() {
    let h = Harness::on();
    h.mesh()
        .put_failures
        .insert(EDGE_KEY.to_string(), usize::MAX);
    h.service.check_once().await;
    assert_eq!(
        h.state().last_attempt.unwrap().stage.as_deref(),
        Some("store")
    );
    h.mesh().put_failures.clear();
    h.advance(HOUR + 1);
    h.service.check_once().await;
    assert_eq!(h.ca().log.new_orders.len(), 2, "the backoff, not the cap");
    assert_eq!(h.state().last_attempt.unwrap().result, "installed");
}

// ---------------------------------------------------------------------------------------------
// An edge that answers an error, the alerts' order, and the CA's roots
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_edge_that_answers_an_error_is_a_configuration_error() {
    let h = Harness::on();
    h.mesh().edge_error = Some("no public listener on this hive".to_string());
    let now = h.now();
    assert_eq!(
        h.service.check_once().await,
        now + 6 * HOUR + 900,
        "no 10-min loop"
    );
    h.advance(3 * HOUR);
    h.service.check_once().await;
    assert!(h.alerts().is_empty(), "not unreachable");
    let status = h.service.status().await;
    assert!(status["edge"]["config_error"]
        .as_str()
        .unwrap()
        .contains("no public listener"));
    assert!(status["edge"]["unreachable_since_at"].is_null());
    assert!(h.ca().log.new_orders.is_empty());
}

#[tokio::test]
async fn closed_rows_go_before_opened_ones() {
    let h = Harness::absent();
    h.service.check_once().await;
    assert_eq!(
        h.alerts(),
        vec![("edge_without_certificate".to_string(), "open".to_string())]
    );
    // Closes and opens again while the orchestrator does not answer.
    h.mesh().alerts_down = true;
    let now = h.now();
    let (chain, key) = h.pki.seeded(&[NAME], now - DAY, now + 80 * DAY);
    h.mesh().seed(&chain, &key);
    h.service.check_once().await;
    {
        let mut mesh = h.mesh();
        mesh.vault.remove(EDGE_KEY);
        mesh.serve_current();
    }
    h.service.check_once().await;
    h.mesh().alerts_down = false;
    h.service.check_once().await;
    assert_eq!(
        h.alerts(),
        vec![
            ("edge_without_certificate".to_string(), "open".to_string()),
            ("edge_without_certificate".to_string(), "closed".to_string()),
            ("edge_without_certificate".to_string(), "open".to_string()),
        ]
    );
}

#[tokio::test]
async fn a_row_the_orchestrator_already_holds_counts_as_delivered() {
    let h = Harness::absent();
    h.mesh().alerts_duplicate = true;
    h.service.check_once().await;
    assert!(h.state().conditions["edge_without_certificate"].alerted);
    h.service.check_once().await;
    assert_eq!(h.alerts().len(), 1);
}

#[tokio::test]
async fn the_ca_clients_trust_the_platform_roots_and_the_anchor() {
    let pki = TestPki::new("Private CA");
    let system = TestPki::new("System");
    let platform = || Ok(vec![system.root_der.clone()]);
    let anchor = std::slice::from_ref(&pki.root_der);
    let roots = ca_roots(anchor, platform).expect("roots");
    assert_eq!(roots, vec![system.root_der.clone(), pki.root_der.clone()]);
    // Without platform roots the anchor alone is trusted; with neither, nothing is.
    assert_eq!(
        ca_roots(anchor, || Err("no ca-certificates".to_string())).expect("anchor"),
        vec![pki.root_der.clone()]
    );
    assert!(ca_roots(&[], || Err("no ca-certificates".to_string())).is_err());
    let ca = InstantAcmeCa::new(anchor, platform);
    assert!(ca.http.is_ok(), "{:?}", ca.http.as_ref().err());
    assert!(ca.tls.is_ok(), "{:?}", ca.tls.as_ref().err());
    assert!(ca.account_builder().is_ok());
}
