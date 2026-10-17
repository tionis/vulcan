use super::*;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};
use std::fmt::Write as _;
use std::fs;

/// An armored signature as Git's continuation lines.
fn continuation(signature: &str) -> String {
    let mut header = String::new();
    for line in signature.trim_end().lines() {
        let _ = writeln!(header, " {line}");
    }
    header
}

const ADMIN: &str = "admin";

fn key() -> PrivateKey {
    PrivateKey::random(&mut ssh_key::rand_core::OsRng, Algorithm::Ed25519).expect("key")
}

fn public_line(principal: &str, key: &PrivateKey) -> String {
    format!(
        "{principal} {}\n",
        key.public_key().to_openssh().expect("public key")
    )
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_string()
}

fn git_with_input(directory: &Path, arguments: &[&str], input: &[u8]) -> String {
    let mut child = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run git");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_string()
}

struct Fixture {
    dir: tempfile::TempDir,
    repo: PathBuf,
    vault: PathBuf,
    admin: PrivateKey,
    ledger: ScheduleLedger,
}

const VAULT_KEY: &str = "vault";

impl Fixture {
    fn new() -> Self {
        Self::with_prefix("")
    }

    /// A repository whose vault lives at `prefix` inside it.
    fn with_prefix(prefix: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).expect("repo");
        git(&repo, &["init", "-q", "-b", "main"]);
        let vault = if prefix.is_empty() {
            repo.clone()
        } else {
            repo.join(prefix)
        };
        fs::create_dir_all(vault.join(".vulcan")).expect("vault");
        let admin = key();
        let fixture = Self {
            ledger: ScheduleLedger::open(&dir.path().join("state")).expect("ledger"),
            dir,
            repo,
            vault,
            admin,
        };
        fixture.write_signers(&public_line(ADMIN, &fixture.admin));
        fixture
    }

    fn signers_path(&self) -> PathBuf {
        self.dir.path().join("execution-signers")
    }

    fn write_signers(&self, text: &str) {
        let path = self.signers_path();
        fs::write(&path, text).expect("signers");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
        }
    }

    fn config(&self) -> ExecutionTrustConfig {
        ExecutionTrustConfig::new(self.signers_path(), vec![ADMIN.to_string()], None)
            .expect("config")
    }

    fn repository(&self) -> TrustRepository {
        TrustRepository {
            vault: self.vault.clone(),
            git_dir: None,
            work_tree: None,
        }
    }

    fn write_jobs(&self, jobs: &[(&str, &str)]) {
        let mut text = String::from(
            "[git]\nauto_commit = true\nstrategy = \"batched\"\n\n[schedules]\nversion = 1\n",
        );
        for (id, trigger) in jobs {
            let _ = write!(
                text,
                "\n[[schedules.jobs]]\nid = \"{id}\"\ntarget = {{ builtin = \"batch-commit\" }}\nnodes = [\"vdev1_node\"]\ntrigger = {{ every = \"{trigger}\" }}\nauthority = \"unrestricted\"\n"
            );
        }
        fs::write(self.vault.join(".vulcan/config.toml"), text).expect("config");
    }

    fn write_note(&self, text: &str) {
        fs::write(self.vault.join("note.md"), text).expect("note");
    }

    /// Commits the work tree on `HEAD`, signed by `signer` when given.
    fn commit(&self, signer: Option<&PrivateKey>) -> String {
        let head = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .current_dir(&self.repo)
            .output()
            .expect("run git");
        let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
        let parents = if head.is_empty() { vec![] } else { vec![head] };
        self.commit_with_parents(signer, &parents)
    }

    fn commit_with_parents(&self, signer: Option<&PrivateKey>, parents: &[String]) -> String {
        git(&self.repo, &["add", "-A"]);
        let tree = git(&self.repo, &["write-tree"]);
        let mut payload = format!("tree {tree}\n");
        for parent in parents {
            let _ = writeln!(payload, "parent {parent}");
        }
        let message = format!("commit {}\n", ulid::Ulid::new());
        payload.push_str("author T <t@example.invalid> 1700000000 +0000\n");
        payload.push_str("committer T <t@example.invalid> 1700000000 +0000\n");
        let object = match signer {
            Some(signer) => {
                let unsigned = format!("{payload}\n{message}");
                let signature = signer
                    .sign("git", HashAlg::Sha512, unsigned.as_bytes())
                    .expect("sign")
                    .to_pem(LineEnding::LF)
                    .expect("pem");
                let mut header = String::from("gpgsig");
                for line in signature.trim_end().lines() {
                    let _ = writeln!(header, " {line}");
                }
                format!("{payload}{header}\n{message}")
            }
            None => format!("{payload}\n{message}"),
        };
        let oid = git_with_input(
            &self.repo,
            &["hash-object", "-t", "commit", "-w", "--stdin"],
            object.as_bytes(),
        );
        git(&self.repo, &["update-ref", "HEAD", &oid]);
        git(&self.repo, &["reset", "-q", "--hard", &oid]);
        oid
    }

    fn resolve(&self) -> TrustResolution {
        resolve_trust_refusing(
            &self.config(),
            &self.repository(),
            &self.ledger,
            VAULT_KEY,
            &[],
        )
    }

    fn enable(&self) -> TrustCheckpoint {
        self.pin(None, "enabled").expect("enable")
    }

    fn pin(&self, commit: Option<&str>, how: &str) -> Result<TrustCheckpoint, AppError> {
        pin_checkpoint_refusing(
            &self.config(),
            &self.repository(),
            &self.ledger,
            VAULT_KEY,
            &CheckpointPin {
                commit,
                how,
                now_ms: 1,
                dry_run: false,
            },
            &[],
        )
    }

    /// Resolves, records any advance, and returns the resolution.
    fn reconcile(&self) -> TrustResolution {
        let resolution = self.resolve();
        record_advance(&resolution, &self.ledger, VAULT_KEY, 2).expect("advance");
        resolution
    }
}

fn state(resolution: &TrustResolution, id: &str) -> TrustState {
    if let Some(block) = &resolution.blocked {
        return block.state;
    }
    resolution.job(id).map_or_else(
        || panic!("job {id} missing: {resolution:?}"),
        |job| job.state,
    )
}

#[test]
fn unsigned_note_edits_after_a_signed_definition_keep_it_authorized() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    let signed = fx.commit(Some(&fx.admin));
    assert_eq!(fx.enable().commit, signed);
    fx.write_note("one");
    fx.commit(None);
    fx.write_note("two");
    fx.commit(None);
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::Authorized);
    assert!(resolution.authorize("nightly").is_ok());
    assert_eq!(resolution.schedules.jobs.len(), 1, "jobs come from the tip");
    assert!(!resolution.job("nightly").expect("job").pending);
}

#[test]
fn an_unsigned_job_change_blocks_only_that_job_until_it_is_signed() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d"), ("hourly", "1h")]);
    fx.commit(Some(&fx.admin));
    fx.enable();
    fx.write_jobs(&[("nightly", "1d"), ("hourly", "2h")]);
    fx.commit(None);
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::Authorized);
    assert_eq!(state(&resolution, "hourly"), TrustState::UnsignedChange);
    assert!(resolution.authorize("hourly").is_err());

    fx.commit(Some(&fx.admin));
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "hourly"), TrustState::Authorized);
}

#[test]
fn an_old_definition_copied_back_unsigned_is_rejected() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(Some(&fx.admin));
    fx.enable();
    // The administrator changes the job; an attacker restores the old one.
    fx.write_jobs(&[("nightly", "7d")]);
    fx.commit(Some(&fx.admin));
    fx.reconcile();
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(None);
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::UnsignedChange);
}

#[test]
fn removing_a_job_without_a_signature_stops_it_and_adding_one_needs_a_signature() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d"), ("hourly", "1h")]);
    fx.commit(Some(&fx.admin));
    fx.enable();
    fx.write_jobs(&[("nightly", "1d"), ("added", "1h")]);
    fx.commit(None);
    let resolution = fx.reconcile();
    assert!(resolution.job("hourly").is_none());
    assert!(resolution.authorize("hourly").is_err());
    assert_eq!(state(&resolution, "added"), TrustState::UnsignedChange);
    assert_eq!(state(&resolution, "nightly"), TrustState::Authorized);
}

#[test]
fn merges_of_differently_signed_definitions_conflict_unless_the_merge_is_signed() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    let base = fx.commit(Some(&fx.admin));
    fx.enable();
    fx.write_jobs(&[("nightly", "2d")]);
    let left = fx.commit(Some(&fx.admin));
    git(&fx.repo, &["reset", "-q", "--hard", &base]);
    fx.write_jobs(&[("nightly", "3d")]);
    let right = fx.commit(Some(&fx.admin));
    // An unsigned merge keeping one side's definition.
    fx.write_jobs(&[("nightly", "2d")]);
    fx.commit_with_parents(None, &[left.clone(), right.clone()]);
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::Conflicted);
    assert_eq!(resolution.frontier.len(), 2);

    let merged = fx.commit_with_parents(Some(&fx.admin), &[left, right]);
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::Authorized);
    assert_eq!(
        fx.ledger
            .trust_checkpoint(VAULT_KEY)
            .expect("checkpoint")
            .expect("set")
            .commit,
        merged,
        "a single signed frontier becomes the checkpoint"
    );
}

#[test]
fn history_without_the_checkpoint_is_a_rollback_until_accepted() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    let old = fx.commit(Some(&fx.admin));
    fx.enable();
    fx.write_jobs(&[("nightly", "7d")]);
    fx.commit(Some(&fx.admin));
    fx.reconcile();
    // A force-push back to the old signed commit.
    git(&fx.repo, &["reset", "-q", "--hard", &old]);
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::RolledBack);

    fx.pin(Some(&old), "accepted").expect("accept");
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::Authorized);
}

#[test]
fn a_removed_signer_authorizes_nothing_and_older_signatures_do_not_return() {
    let fx = Fixture::new();
    let other = key();
    fx.write_signers(&(public_line(ADMIN, &fx.admin) + &public_line(ADMIN, &other)));
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(Some(&other));
    fx.enable();
    fx.write_jobs(&[("nightly", "7d")]);
    fx.commit(Some(&fx.admin));
    fx.reconcile();
    // The checkpoint's signer is removed; the older key's commit must not
    // become authoritative again.
    fx.write_signers(&public_line(ADMIN, &other));
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::Unsigned);
    fx.commit(Some(&other));
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::Authorized);
}

#[test]
fn a_missing_checkpoint_blocks_until_one_is_pinned() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(Some(&fx.admin));
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::NoCheckpoint);
    assert!(fx
        .ledger
        .trust_checkpoint(VAULT_KEY)
        .expect("read")
        .is_none());
    fx.enable();
    assert_eq!(state(&fx.reconcile(), "nightly"), TrustState::Authorized);
}

#[test]
fn enabling_without_a_signed_commit_fails_and_unsigned_history_authorizes_nothing() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(None);
    assert!(fx.pin(None, "enabled").is_err());
    assert!(fx.pin(Some("HEAD"), "accepted").is_err());
}

#[test]
fn device_keys_and_unprotected_or_in_vault_signer_files_are_refused() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(Some(&fx.admin));
    fx.enable();
    let refused = resolve_trust_refusing(
        &fx.config(),
        &fx.repository(),
        &fx.ledger,
        VAULT_KEY,
        &[fx.admin.public_key().clone()],
    );
    assert_eq!(state(&refused, "nightly"), TrustState::Misconfigured);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(fx.signers_path(), fs::Permissions::from_mode(0o666)).expect("mode");
        assert_eq!(state(&fx.resolve(), "nightly"), TrustState::Misconfigured);
        fs::set_permissions(fx.signers_path(), fs::Permissions::from_mode(0o600)).expect("mode");
    }

    let inside = fx.vault.join("signers");
    fs::write(&inside, public_line(ADMIN, &fx.admin)).expect("signers");
    let config = ExecutionTrustConfig::new(inside, vec![ADMIN.to_string()], None).expect("config");
    let resolution = resolve_trust_refusing(&config, &fx.repository(), &fx.ledger, VAULT_KEY, &[]);
    assert_eq!(state(&resolution, "nightly"), TrustState::Misconfigured);
}

#[test]
fn a_vault_in_a_repository_subdirectory_reads_its_own_config() {
    let fx = Fixture::with_prefix("docs");
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(Some(&fx.admin));
    fx.enable();
    let resolution = fx.reconcile();
    assert_eq!(state(&resolution, "nightly"), TrustState::Authorized);
    assert_eq!(resolution.schedules.jobs.len(), 1);
}

#[test]
fn uncommitted_edits_are_pending_and_have_no_effect() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(Some(&fx.admin));
    fx.enable();
    fx.write_jobs(&[("nightly", "1h")]);
    let resolution = fx.reconcile();
    let job = resolution.job("nightly").expect("job");
    assert_eq!(job.state, TrustState::Authorized);
    assert!(job.pending);
    let tip_job = resolution.schedules.job("nightly").expect("tip job");
    assert!(matches!(
        tip_job.trigger,
        vulcan_core::schedules::time::Trigger::Every {
            every_seconds: 86_400,
            ..
        }
    ));
}

#[test]
fn changing_the_git_table_is_a_batch_commit_definition_change() {
    let fx = Fixture::new();
    fx.write_jobs(&[("nightly", "1d")]);
    fx.commit(Some(&fx.admin));
    fx.enable();
    let config = fs::read_to_string(fx.vault.join(".vulcan/config.toml")).expect("config");
    fs::write(
        fx.vault.join(".vulcan/config.toml"),
        config.replace("[git]\n", "[git]\nbatch_messages = \"agent\"\n"),
    )
    .expect("config");
    fx.commit(None);
    assert_eq!(
        state(&fx.reconcile(), "nightly"),
        TrustState::UnsignedChange
    );
}

#[test]
fn a_plain_directory_is_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path().join("vault");
    fs::create_dir_all(&vault).expect("vault");
    let signers = dir.path().join("signers");
    fs::write(&signers, "").expect("signers");
    let config = ExecutionTrustConfig::new(signers, vec![ADMIN.to_string()], None).expect("config");
    let ledger = ScheduleLedger::open(&dir.path().join("state")).expect("ledger");
    let resolution = resolve_trust_refusing(
        &config,
        &TrustRepository {
            vault,
            git_dir: None,
            work_tree: None,
        },
        &ledger,
        VAULT_KEY,
        &[],
    );
    assert_eq!(
        resolution.blocked.expect("blocked").state,
        TrustState::Unsupported
    );
}

#[test]
fn signatures_verify_only_for_the_signed_bytes_and_one_header() {
    let admin = key();
    let signers = vec![AllowedSigner {
        principal: ADMIN.to_string(),
        key: admin.public_key().clone(),
    }];
    let payload = "tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor T <t@x> 1 +0000\ncommitter T <t@x> 1 +0000\n\nmessage\n";
    let signature = admin
        .sign("git", HashAlg::Sha512, payload.as_bytes())
        .expect("sign")
        .to_pem(LineEnding::LF)
        .expect("pem");
    let header = continuation(&signature);
    let signed = payload.replacen("\n\n", &format!("\ngpgsig{header}\n"), 1);
    assert_eq!(
        verify_commit(signed.as_bytes(), &signers).as_deref(),
        Some(ADMIN)
    );
    // Any change to the signed bytes fails.
    assert_eq!(
        verify_commit(signed.replace("message", "massage").as_bytes(), &signers),
        None
    );
    // Two signature headers are refused.
    let doubled = signed.replacen("\ngpgsig", &format!("\ngpgsig{header}gpgsig"), 1);
    assert_eq!(verify_commit(doubled.as_bytes(), &signers), None);
    // Another namespace is not a commit signature.
    let file_signature = admin
        .sign("file", HashAlg::Sha512, payload.as_bytes())
        .expect("sign")
        .to_pem(LineEnding::LF)
        .expect("pem");
    let header = continuation(&file_signature);
    let wrong = payload.replacen("\n\n", &format!("\ngpgsig{header}\n"), 1);
    assert_eq!(verify_commit(wrong.as_bytes(), &signers), None);
    // An OpenPGP signature is not accepted.
    let pgp = payload.replacen(
        "\n\n",
        "\ngpgsig -----BEGIN PGP SIGNATURE-----\n abc\n -----END PGP SIGNATURE-----\n\n",
        1,
    );
    assert_eq!(verify_commit(pgp.as_bytes(), &signers), None);
}

#[test]
fn allowed_signer_lines_keep_only_supported_entries_for_listed_principals() {
    let admin = key();
    let key_text = admin.public_key().to_openssh().expect("public");
    let text = format!(
        "# comment\n\
         admin {key_text}\n\
         other {key_text}\n\
         admin namespaces=\"file\" {key_text}\n\
         admin,ops namespaces=\"git,file\" {key_text}\n\
         admin cert-authority {key_text}\n\
         ad* {key_text}\n\
         admin ssh-rsa AAAA\n"
    );
    let loaded = parse_allowed_signers(&text, &[ADMIN.to_string()]);
    assert_eq!(
        loaded.signers.len(),
        2,
        "plain and git-namespaced admin lines"
    );
    assert_eq!(loaded.diagnostics.len(), 3, "{:?}", loaded.diagnostics);
    assert!(loaded.diagnostics[0].starts_with("line 6:"));
}

#[test]
fn configuration_requires_an_absolute_path_and_plain_principals() {
    assert!(ExecutionTrustConfig::new("relative".into(), vec![ADMIN.into()], None).is_err());
    assert!(ExecutionTrustConfig::new("/abs".into(), vec![], None).is_err());
    assert!(ExecutionTrustConfig::new("/abs".into(), vec!["a*".into()], None).is_err());
    assert!(
        ExecutionTrustConfig::new("/abs".into(), vec![ADMIN.into()], Some("-x".into())).is_err()
    );
    let config = ExecutionTrustConfig::new("/abs".into(), vec![ADMIN.into()], None).expect("ok");
    assert_eq!(config.source, "HEAD");
    let text = toml::to_string(&config).expect("toml");
    assert!(text.contains("policy = \"trusted-commit\""));
}
