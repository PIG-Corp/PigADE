//! Native Git hook wrappers and the prepared/committed guard for workspace commits.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const PLAN_FILE: &str = "hook-plan.json";
const RUN_FILE: &str = "run.json";
const PREPARED_FILE: &str = "prepared.json";
const COMMITTED_FILE: &str = "committed.json";
const MAX_PLAN_BYTES: u64 = 64 * 1024;
const MAX_HOOK_INPUT: usize = 64 * 1024;
const MAX_COMMIT_BYTES: usize = 256 * 1024;
const HOOKS: &[&str] = &[
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "post-rewrite",
    "reference-transaction",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HookPlan {
    pub ticket: Uuid,
    pub cwd: PathBuf,
    pub git: PathBuf,
    pub expected_head: Option<String>,
    pub head_ref: Option<String>,
    pub expected_tree: String,
    pub hooks_dir: PathBuf,
    pub helper_fd: i32,
    pub git_identity: (u64, u64),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRecord {
    version: u8,
    ticket: Uuid,
    pid: u32,
    boot_id: String,
    start_ticks: u64,
    git_identity: (u64, u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    version: u8,
    ticket: Uuid,
    commit: String,
    head_ref: String,
    old_head: Option<String>,
    tree: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefUpdate {
    old: String,
    new: String,
    name: String,
}

/// Create a fresh, private hook directory for one native `git commit` invocation.
pub(super) fn prepare_hooks(operation: &Path, plan: &HookPlan) -> Result<PathBuf> {
    validate_plan(plan)?;
    validate_private_directory(operation)?;
    let hooks = operation.join("hooks");
    fs::create_dir(&hooks).context("create commit hook directory")?;
    fs::set_permissions(&hooks, fs::Permissions::from_mode(0o700))?;
    for name in HOOKS {
        let path = hooks.join(name);
        let mut body = format!(
            "#!/bin/sh\nexec /proc/self/fd/{} __git_commit_hook ",
            plan.helper_fd
        )
        .into_bytes();
        body.extend(shell_quote(operation.as_os_str().as_bytes()));
        body.push(b' ');
        body.extend(shell_quote(name.as_bytes()));
        body.extend_from_slice(b" \"$@\"\n");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("create {name} wrapper"))?;
        file.write_all(&body)?;
        file.sync_all()?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    File::open(&hooks)?.sync_all()?;
    File::open(operation)?.sync_all()?;
    Ok(hooks)
}

/// Replace this process with the requested native Git commit. A preexisting run marker
/// means that the previous outcome must be inspected rather than retried.
pub(super) fn run(operation: &Path) -> Result<()> {
    let plan = read_plan(operation)?;
    validate_plan(&plan)?;
    let hooks = operation.join("hooks");
    validate_private_directory(&hooks)?;
    ensure!(
        !operation.join(RUN_FILE).exists(),
        "commit attempt already exists; outcome must be inspected"
    );

    let pid = std::process::id();
    let record = RunRecord {
        version: 1,
        ticket: plan.ticket,
        pid,
        boot_id: read_boot_id()?,
        start_ticks: process_start_ticks(pid)?,
        git_identity: plan.git_identity,
    };
    write_create_new(operation, RUN_FILE, &record)?;

    let message = operation.join("message");
    ensure_private_file(&message, false)?;
    let mut command = Command::new(&plan.git);
    command
        .current_dir(&plan.cwd)
        .arg("-c")
        .arg(prefixed_path("core.hooksPath=", &hooks))
        .arg("commit")
        .arg(prefixed_path("--file=", &message))
        .arg("--cleanup=verbatim")
        .arg("--no-edit")
        .arg("--no-status");
    let error = command.exec();
    Err(error).context("exec native Git commit")
}

/// Run one intercepted hook and return the user's hook exit code, or zero on success.
pub(super) fn hook(operation: &Path, name: &str, args: &[String]) -> Result<i32> {
    ensure!(HOOKS.contains(&name), "unsupported Git hook");
    let plan = read_plan(operation)?;
    validate_plan(&plan)?;
    let owner = read_run(operation, &plan)?;
    let direct_owner = direct_commit_process(&owner, &plan)?;
    let original = plan.hooks_dir.join(name);
    if name != "reference-transaction" {
        return if executable_file(&original)? {
            invoke_original_inherit(&original, args)
        } else {
            Ok(0)
        };
    }
    ensure!(
        args.len() == 1,
        "invalid reference-transaction state arguments"
    );
    let input = read_stdin_limited(MAX_HOOK_INPUT)?;
    // Native commit also runs cleanup transactions for pseudo-refs such as
    // AUTO_MERGE. They are not the publication transaction being guarded.
    // Never reject or issue a publication proof for those unrelated updates.
    let guarded = direct_owner && mentions_reviewed_ref(&input, target_ref(&plan));
    if !guarded {
        return if executable_file(&original)? {
            invoke_original(&original, args, &input)
        } else {
            Ok(0)
        };
    }
    if name == "reference-transaction" && direct_owner && args[0] == "committed" {
        let updates = parse_updates(&input)?;
        let proof = validate_committed(&plan, operation, &updates)?;
        write_idempotent_proof(operation, COMMITTED_FILE, &proof)?;
        return if executable_file(&original)? {
            invoke_original(&original, args, &input)
        } else {
            Ok(0)
        };
    }
    let user_code = if executable_file(&original)? {
        invoke_original(&original, args, &input)?
    } else {
        0
    };
    if !direct_owner {
        return Ok(user_code);
    }
    match args[0].as_str() {
        "preparing" => Ok(user_code),
        "prepared" => {
            if user_code != 0 {
                return Ok(user_code);
            }
            let updates = parse_updates(&input)?;
            let proof = validate_prepared(&plan, &updates)?;
            write_idempotent_proof(operation, PREPARED_FILE, &proof)?;
            Ok(0)
        }
        "aborted" => Ok(user_code),
        _ => bail!("unknown reference-transaction state"),
    }
}

/// Return a published commit OID only when the durable committed proof matches ticket.
pub(super) fn committed(operation: &Path, ticket: Uuid) -> Result<Option<String>> {
    let plan = read_plan(operation)?;
    ensure!(plan.ticket == ticket, "commit ticket mismatch");
    let Some(proof) = read_optional_proof(operation, COMMITTED_FILE)? else {
        return Ok(None);
    };
    validate_proof(&plan, &proof)?;
    ensure!(
        read_optional_proof(operation, PREPARED_FILE)?.as_ref() == Some(&proof),
        "committed proof has no matching prepared proof"
    );
    validate_proof_commit(&plan, &proof)?;
    Ok(Some(proof.commit))
}

/// Return the candidate commit recorded after refs were locked and validated.
/// Presence alone does not prove publication; consult `committed` for that.
pub(super) fn prepared(operation: &Path, ticket: Uuid) -> Result<Option<String>> {
    let plan = read_plan(operation)?;
    ensure!(plan.ticket == ticket, "commit ticket mismatch");
    let Some(proof) = read_optional_proof(operation, PREPARED_FILE)? else {
        return Ok(None);
    };
    validate_proof(&plan, &proof)?;
    validate_proof_commit(&plan, &proof)?;
    Ok(Some(proof.commit))
}

/// True only while the exact process recorded for this commit attempt is alive.
pub(super) fn running(operation: &Path) -> Result<bool> {
    let plan = read_plan(operation)?;
    let record = match read_optional_run(operation)? {
        Some(record) => record,
        None => return Ok(false),
    };
    if record.ticket != plan.ticket || record.git_identity != plan.git_identity {
        bail!("commit run record does not match plan");
    }
    ensure!(record.version == 1, "unsupported commit run record version");
    if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", record.pid))
        && stat
            .rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().next())
            .is_some_and(|state| matches!(state, "Z" | "X"))
    {
        return Ok(false);
    }
    let identity = match process_identity(record.pid) {
        Ok(identity) => identity,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error).context("verify commit process identity"),
    };
    if identity.0 != record.boot_id || identity.1 != record.start_ticks {
        return Ok(false);
    }
    Ok(true)
}

fn validate_prepared(plan: &HookPlan, updates: &[RefUpdate]) -> Result<Proof> {
    let target = target_ref(plan);
    let update = target_update(target, updates)?;
    ensure!(
        updates.len() == 1,
        "unexpected additional ref update in commit transaction"
    );
    let zero = "0".repeat(update.new.len());
    let expected_old = plan.expected_head.as_deref().unwrap_or(&zero);
    ensure!(
        update.old == expected_old,
        "reviewed HEAD changed before commit publication"
    );
    ensure!(
        is_oid(&update.new) && update.new.len() == plan.expected_tree.len(),
        "invalid commit object ID"
    );
    ensure!(
        current_head_ref(plan)?.as_deref() == plan.head_ref.as_deref(),
        "HEAD target changed during commit"
    );
    let (tree, parents) = read_commit_header(plan, &update.new)?;
    ensure!(
        tree == plan.expected_tree,
        "commit tree differs from reviewed index tree"
    );
    let expected_parents = plan.expected_head.iter().cloned().collect::<Vec<_>>();
    ensure!(
        parents == expected_parents,
        "commit parent differs from reviewed HEAD"
    );
    let proof = Proof {
        version: 1,
        ticket: plan.ticket,
        commit: update.new.clone(),
        head_ref: target.to_owned(),
        old_head: plan.expected_head.clone(),
        tree,
    };
    validate_proof(plan, &proof)?;
    Ok(proof)
}

fn validate_committed(plan: &HookPlan, operation: &Path, updates: &[RefUpdate]) -> Result<Proof> {
    let prepared = read_optional_proof(operation, PREPARED_FILE)?
        .context("prepared commit proof is missing")?;
    validate_proof(plan, &prepared)?;
    let update = target_update(target_ref(plan), updates)?;
    ensure!(
        updates.len() == 1,
        "unexpected additional ref update in committed transaction"
    );
    ensure!(
        update.new == prepared.commit,
        "published commit differs from prepared proof"
    );
    ensure!(
        update.old
            == prepared
                .old_head
                .as_deref()
                .unwrap_or(&"0".repeat(update.new.len())),
        "published old HEAD differs from prepared proof"
    );
    Ok(prepared)
}

fn target_ref(plan: &HookPlan) -> &str {
    plan.head_ref.as_deref().unwrap_or("HEAD")
}

fn target_update<'a>(target: &str, updates: &'a [RefUpdate]) -> Result<&'a RefUpdate> {
    let mut found = updates.iter().filter(|update| update.name == target);
    let update = found
        .next()
        .context("expected HEAD ref update is missing")?;
    ensure!(found.next().is_none(), "duplicate HEAD ref update");
    Ok(update)
}

fn mentions_reviewed_ref(input: &[u8], target: &str) -> bool {
    input.split(|byte| *byte == b'\n').any(|line| {
        line.rsplit(|byte| *byte == b' ')
            .next()
            .is_some_and(|name| name == target.as_bytes() || name == b"HEAD")
    })
}

fn parse_updates(input: &[u8]) -> Result<Vec<RefUpdate>> {
    ensure!(!input.is_empty(), "reference-transaction input is empty");
    let text = std::str::from_utf8(input).context("reference-transaction input is not UTF-8")?;
    let mut updates = Vec::new();
    for line in text.lines() {
        let fields = line.split(' ').collect::<Vec<_>>();
        ensure!(fields.len() == 3, "malformed reference-transaction update");
        let (old, new, name) = (fields[0], fields[1], fields[2]);
        ensure!(
            is_oid(old) && is_oid(new) && !name.is_empty(),
            "invalid reference-transaction update"
        );
        ensure!(
            !name.starts_with("ref:"),
            "symbolic transaction values are not supported here"
        );
        updates.push(RefUpdate {
            old: old.to_owned(),
            new: new.to_owned(),
            name: name.to_owned(),
        });
    }
    ensure!(!updates.is_empty(), "no reference updates found");
    Ok(updates)
}

fn read_commit_header(plan: &HookPlan, oid: &str) -> Result<(String, Vec<String>)> {
    let mut child = Command::new(&plan.git)
        .current_dir(&plan.cwd)
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .arg("--no-replace-objects")
        .arg("cat-file")
        .arg("commit")
        .arg(oid)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("read proposed commit object")?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .context("commit object output unavailable")?
        .take((MAX_COMMIT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_COMMIT_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        bail!("proposed commit object exceeds limit");
    }
    let status = child.wait()?;
    ensure!(status.success(), "cannot read proposed commit object");
    let header_end = bytes
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .context("commit object has no header terminator")?;
    let header = &bytes[..header_end];
    let mut tree = None;
    let mut parents = Vec::new();
    for line in header.split(|byte| *byte == b'\n') {
        if let Some(value) = line.strip_prefix(b"tree ") {
            let value = std::str::from_utf8(value).context("commit tree OID is not ASCII")?;
            ensure!(
                tree.replace(value.to_owned()).is_none(),
                "commit has duplicate tree header"
            );
        } else if let Some(value) = line.strip_prefix(b"parent ") {
            let value = std::str::from_utf8(value).context("commit parent OID is not ASCII")?;
            ensure!(is_oid(value), "commit has invalid parent OID");
            parents.push(value.to_owned());
        }
    }
    let tree = tree.context("commit tree header is missing")?;
    ensure!(
        is_oid(&tree) && tree.len() == plan.expected_tree.len(),
        "commit has invalid tree OID"
    );
    Ok((tree, parents))
}

fn current_head_ref(plan: &HookPlan) -> Result<Option<String>> {
    let output = Command::new(&plan.git)
        .current_dir(&plan.cwd)
        .arg("symbolic-ref")
        .arg("-q")
        .arg("HEAD")
        .output()
        .context("inspect current HEAD target")?;
    if output.status.success() {
        let value = String::from_utf8(output.stdout)?.trim().to_owned();
        ensure!(
            value.starts_with("refs/"),
            "HEAD symbolic target is invalid"
        );
        Ok(Some(value))
    } else {
        ensure!(
            output.status.code() == Some(1),
            "cannot inspect current HEAD target"
        );
        Ok(None)
    }
}

fn invoke_original(path: &Path, args: &[String], input: &[u8]) -> Result<i32> {
    let mut child = Command::new(path);
    child
        .arg0(path.as_os_str())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut process = child
        .spawn()
        .with_context(|| format!("run original hook {}", path.display()))?;
    process
        .stdin
        .take()
        .context("original hook stdin unavailable")?
        .write_all(input)?;
    let status = process.wait()?;
    Ok(status.code().unwrap_or(1))
}

fn invoke_original_inherit(path: &Path, args: &[String]) -> Result<i32> {
    let status = Command::new(path)
        .arg0(path.as_os_str())
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("run original hook {}", path.display()))?;
    Ok(status.code().unwrap_or(1))
}

fn read_stdin_limited(limit: usize) -> Result<Vec<u8>> {
    let mut input = Vec::new();
    std::io::stdin()
        .take((limit + 1) as u64)
        .read_to_end(&mut input)?;
    ensure!(input.len() <= limit, "Git hook input exceeds limit");
    Ok(input)
}

fn executable_file(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.is_file() && meta.mode() & 0o111 != 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("inspect original hook {}", path.display()))
        }
    }
}

fn validate_plan(plan: &HookPlan) -> Result<()> {
    ensure!(
        plan.cwd.is_absolute() && plan.git.is_absolute() && plan.hooks_dir.is_absolute(),
        "commit paths must be absolute"
    );
    ensure!(plan.helper_fd > 2, "invalid helper file descriptor");
    ensure!(is_oid(&plan.expected_tree), "invalid reviewed tree OID");
    if let Some(head) = &plan.expected_head {
        ensure!(
            is_oid(head) && head.len() == plan.expected_tree.len(),
            "invalid reviewed HEAD OID"
        );
    }
    if let Some(reference) = &plan.head_ref {
        ensure!(
            reference.starts_with("refs/heads/") && !reference.contains('\n'),
            "invalid reviewed branch ref"
        );
    }
    ensure!(
        plan.git_identity == file_identity(&plan.git)?,
        "Git executable identity changed"
    );
    Ok(())
}

fn is_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn shell_quote(value: &[u8]) -> Vec<u8> {
    let mut quoted = Vec::with_capacity(value.len() + 2);
    quoted.push(b'\'');
    for byte in value {
        if *byte == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(*byte);
        }
    }
    quoted.push(b'\'');
    quoted
}

fn prefixed_path(prefix: &str, path: &Path) -> std::ffi::OsString {
    let mut value = std::ffi::OsString::from(prefix);
    value.push(path.as_os_str());
    value
}

fn read_plan(operation: &Path) -> Result<HookPlan> {
    validate_private_directory(operation)?;
    let bytes = read_private_file(operation, PLAN_FILE, MAX_PLAN_BYTES)?;
    serde_json::from_slice(&bytes).context("decode commit hook plan")
}

fn read_optional_run(operation: &Path) -> Result<Option<RunRecord>> {
    match read_private_file(operation, RUN_FILE, MAX_PLAN_BYTES) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("decode commit run record")?,
        )),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn read_run(operation: &Path, plan: &HookPlan) -> Result<RunRecord> {
    let record = read_optional_run(operation)?.context("commit run record is missing")?;
    ensure!(
        record.version == 1
            && record.ticket == plan.ticket
            && record.git_identity == plan.git_identity,
        "commit run record does not match plan"
    );
    Ok(record)
}

fn read_optional_proof(operation: &Path, name: &str) -> Result<Option<Proof>> {
    match read_private_file(operation, name, MAX_PLAN_BYTES) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).context("decode commit proof")?,
        )),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn validate_proof(plan: &HookPlan, proof: &Proof) -> Result<()> {
    ensure!(
        proof.version == 1 && proof.ticket == plan.ticket,
        "commit proof ticket mismatch"
    );
    ensure!(
        proof.head_ref == target_ref(plan) && proof.old_head == plan.expected_head,
        "commit proof HEAD binding mismatch"
    );
    ensure!(
        proof.tree == plan.expected_tree && is_oid(&proof.commit),
        "commit proof tree or OID mismatch"
    );
    Ok(())
}

fn validate_proof_commit(plan: &HookPlan, proof: &Proof) -> Result<()> {
    let (tree, parents) = read_commit_header(plan, &proof.commit)?;
    ensure!(
        tree == proof.tree,
        "commit object tree differs from durable proof"
    );
    let expected_parents = plan.expected_head.iter().cloned().collect::<Vec<_>>();
    ensure!(
        parents == expected_parents,
        "commit object parent differs from durable proof"
    );
    Ok(())
}

fn write_idempotent_proof(operation: &Path, name: &str, proof: &Proof) -> Result<()> {
    match write_atomic_proof(operation, name, proof) {
        Ok(()) => Ok(()),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists) =>
        {
            let existing = read_optional_proof(operation, name)?
                .context("existing commit proof disappeared")?;
            ensure!(
                existing == *proof,
                "conflicting commit proof already exists"
            );
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn write_atomic_proof<T: Serialize>(operation: &Path, name: &str, value: &T) -> Result<()> {
    let temporary_name = format!(".{name}.tmp-{}", Uuid::new_v4());
    let temporary = operation.join(&temporary_name);
    let destination = operation.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temporary)
        .context("create temporary commit proof")?;
    serde_json::to_writer(&mut file, value)?;
    file.sync_all()?;
    drop(file);
    let result = fs::hard_link(&temporary, &destination).with_context(|| format!("publish {name}"));
    let _ = fs::remove_file(&temporary);
    result?;
    File::open(operation)?.sync_all()?;
    Ok(())
}

fn write_create_new<T: Serialize>(operation: &Path, name: &str, value: &T) -> Result<()> {
    let path = operation.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)
        .with_context(|| format!("create {name}"))?;
    serde_json::to_writer(&mut file, value)?;
    file.sync_all()?;
    File::open(operation)?.sync_all()?;
    Ok(())
}

fn read_private_file(directory: &Path, name: &str, limit: u64) -> Result<Vec<u8>> {
    let path = directory.join(name);
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .with_context(|| format!("open {name}"))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == unsafe { libc::getuid() },
        "unsafe {name}"
    );
    ensure!(
        metadata.mode() & 0o077 == 0 && metadata.len() <= limit,
        "{name} is not private or exceeds limit"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "{name} exceeds limit");
    Ok(bytes)
}

fn ensure_private_file(path: &Path, executable: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.uid() == unsafe { libc::getuid() },
        "unsafe file {}",
        path.display()
    );
    ensure!(
        meta.mode() & 0o077 == 0,
        "file is not private: {}",
        path.display()
    );
    if executable {
        ensure!(meta.mode() & 0o111 != 0, "hook directory is not accessible");
    }
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::getuid() },
        "unsafe commit operation directory"
    );
    ensure!(
        meta.mode() & 0o077 == 0,
        "commit operation directory is not private"
    );
    Ok(())
}

fn process_identity(pid: u32) -> Result<(String, u64)> {
    Ok((read_boot_id()?, process_start_ticks(pid)?))
}

fn read_boot_id() -> Result<String> {
    Ok(fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}

fn process_start_ticks(pid: u32) -> Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(") ")
        .context("invalid process stat")?
        .1
        .split_whitespace()
        .nth(19)
        .context("missing process start time")?
        .parse()
        .context("invalid process start time")
}

fn process_executable_identity(pid: u32) -> Result<(u64, u64)> {
    let meta = fs::metadata(format!("/proc/{pid}/exe"))?;
    Ok((meta.dev(), meta.ino()))
}

fn direct_commit_process(record: &RunRecord, plan: &HookPlan) -> Result<bool> {
    let ppid = fs::read_to_string("/proc/self/status")?
        .lines()
        .find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.trim().parse::<u32>().ok())
        })
        .context("cannot determine hook parent process")?;
    if ppid != record.pid {
        return Ok(false);
    }
    let (boot, ticks) = process_identity(ppid)?;
    ensure!(
        boot == record.boot_id && ticks == record.start_ticks,
        "native commit parent identity changed"
    );
    ensure!(
        process_executable_identity(ppid)? == plan.git_identity,
        "native commit executable changed"
    );
    Ok(true)
}

fn file_identity(path: &Path) -> Result<(u64, u64)> {
    let meta = fs::metadata(path)?;
    ensure!(meta.is_file(), "Git executable is not a regular file");
    Ok((meta.dev(), meta.ino()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_owner_identity_changes_fail_closed_instead_of_delegating_the_guard() {
        let pid = unsafe { libc::getppid() } as u32;
        let (boot_id, start_ticks) = process_identity(pid).unwrap();
        let identity = process_executable_identity(pid).unwrap();
        let mut plan = HookPlan {
            ticket: Uuid::new_v4(),
            cwd: PathBuf::from("/repo"),
            git: PathBuf::from("/usr/bin/git"),
            expected_head: None,
            head_ref: Some("refs/heads/main".into()),
            expected_tree: "b".repeat(40),
            hooks_dir: PathBuf::from("/repo/.git/hooks"),
            helper_fd: 3,
            git_identity: identity,
        };
        let mut record = RunRecord {
            version: 1,
            ticket: plan.ticket,
            pid,
            boot_id,
            start_ticks,
            git_identity: identity,
        };
        assert!(direct_commit_process(&record, &plan).unwrap());
        plan.git_identity = (0, 0);
        assert!(direct_commit_process(&record, &plan).is_err());
        plan.git_identity = identity;
        record.start_ticks += 1;
        assert!(direct_commit_process(&record, &plan).is_err());
        record.pid = std::process::id();
        assert!(!direct_commit_process(&record, &plan).unwrap());
    }

    #[test]
    fn only_reviewed_head_updates_are_guarded_not_native_cleanup_refs() {
        assert!(!mentions_reviewed_ref(
            b"old new AUTO_MERGE\n",
            "refs/heads/main"
        ));
        assert!(!mentions_reviewed_ref(
            b"old new refs/notes/commits\n",
            "refs/heads/main"
        ));
        assert!(mentions_reviewed_ref(
            b"old new refs/heads/main\n",
            "refs/heads/main"
        ));
        assert!(mentions_reviewed_ref(b"old new HEAD\n", "refs/heads/main"));
        assert!(mentions_reviewed_ref(b"old new HEAD\n", "HEAD"));
    }

    #[test]
    fn shell_quote_handles_single_quotes_and_spaces() {
        assert_eq!(shell_quote(b"/tmp/a path/it's"), b"'/tmp/a path/it'\\''s'");
    }

    #[test]
    fn parses_only_well_formed_ref_updates() {
        let oid = "a".repeat(40);
        let new = "b".repeat(40);
        let parsed = parse_updates(format!("{oid} {new} refs/heads/main\n").as_bytes()).unwrap();
        assert_eq!(
            parsed,
            vec![RefUpdate {
                old: oid.clone(),
                new: new.clone(),
                name: "refs/heads/main".into()
            }]
        );
        assert!(parse_updates(b"bad input\n").is_err());
        assert!(
            parse_updates(b"ref:refs/heads/main 0000000000000000000000000000000000000000 HEAD\n")
                .is_err()
        );
        let sha256 = "c".repeat(64);
        assert!(
            parse_updates(format!("{} {sha256} refs/heads/main\n", "0".repeat(64)).as_bytes())
                .is_ok()
        );
        assert!(parse_updates(format!("{oid}  {new} refs/heads/main\n").as_bytes()).is_err());
    }

    #[test]
    fn durable_proof_must_match_ticket_head_and_tree() {
        let plan = HookPlan {
            ticket: Uuid::new_v4(),
            cwd: PathBuf::from("/repo"),
            git: PathBuf::from("/usr/bin/git"),
            expected_head: Some("a".repeat(40)),
            head_ref: Some("refs/heads/main".into()),
            expected_tree: "b".repeat(40),
            hooks_dir: PathBuf::from("/repo/.git/hooks"),
            helper_fd: 3,
            git_identity: (1, 2),
        };
        let proof = Proof {
            version: 1,
            ticket: plan.ticket,
            commit: "c".repeat(40),
            head_ref: "refs/heads/main".into(),
            old_head: plan.expected_head.clone(),
            tree: plan.expected_tree.clone(),
        };
        assert!(validate_proof(&plan, &proof).is_ok());
        let mut mismatched = proof.clone();
        mismatched.tree = "d".repeat(40);
        assert!(validate_proof(&plan, &mismatched).is_err());
        mismatched = proof.clone();
        mismatched.old_head = None;
        assert!(validate_proof(&plan, &mismatched).is_err());
    }
}
