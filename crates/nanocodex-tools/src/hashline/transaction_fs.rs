//! Safe descriptor-relative adaptation of the `OpenAI` Codex Hashline Linux
//! filesystem capability. See the crate `NOTICE` for source provenance.

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::os::unix::fs::MetadataExt as _;
    use std::path::{Component, Path, PathBuf};
    use std::sync::Arc;

    use rustix::fs::{self, AtFlags, Dir, FlockOperation, IFlags, Mode, OFlags, ResolveFlags};
    use serde::{Deserialize, Serialize};

    use super::super::{
        FileMetadata, FunctionCallError, MAX_MUTATIONS, Observed, PreparedMutation, exact_digest,
        model_error,
    };

    const EXT_SUPER_MAGIC: i64 = 0xef53;
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    const CASEFOLD_FLAG: u32 = 0x4000_0000;
    const STATE_DIRECTORY: &str = ".nanocodex";
    const TRANSACTION_DIRECTORY: &str = "hashline-transactions";
    const JOURNAL_VERSION: u32 = 2;
    const MAX_JOURNAL_BYTES: usize = 256 * 1024;
    const MAX_TERMINAL_RECEIPTS: usize = 64;
    const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;

    #[derive(Clone, Debug)]
    pub(in crate::hashline) struct NativeRoot {
        directory: Arc<File>,
        identity: Vec<u8>,
    }

    #[derive(Clone, Debug)]
    struct NativePath {
        root: Arc<File>,
        parent: Arc<File>,
        parent_relative: PathBuf,
        parent_identity: DirectoryIdentity,
        name: OsString,
        model_path: String,
    }

    #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
    struct DirectoryIdentity {
        device: u64,
        inode: u64,
    }

    #[derive(Debug)]
    pub(in crate::hashline) struct TransactionLease {
        root_identity: Vec<u8>,
        covered_parents: BTreeSet<DirectoryIdentity>,
        _locks: Vec<File>,
    }

    #[derive(Debug)]
    pub(in crate::hashline) struct JournalHandle {
        directory: Arc<File>,
        file: File,
        _reservation: File,
        name: String,
        journal: Journal,
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    enum JournalState {
        Preparing,
        Prepared,
        Committing,
        Committed,
        RollingBack,
        RolledBack,
        RecoveryRequired,
        Cleaning,
        Complete,
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    enum TerminalOutcome {
        Committed,
        RolledBack,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct ArtifactEvidence {
        name: String,
        exact_digest: String,
        bytes: usize,
        metadata: Option<FileMetadata>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Journal {
        version: u32,
        transaction_id: String,
        root_identity: String,
        state: JournalState,
        progress: usize,
        rollback_progress: usize,
        rollback_limit: usize,
        outcome: Option<TerminalOutcome>,
        mutations: Vec<JournalMutation>,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(deny_unknown_fields)]
    struct JournalMutation {
        kind: String,
        path: String,
        destination: Option<String>,
        before: Option<ArtifactEvidence>,
        after: Option<ArtifactEvidence>,
    }

    pub(in crate::hashline) fn open_root(
        workspace: &Path,
        root_name: &str,
    ) -> Result<NativeRoot, FunctionCallError> {
        let workspace =
            File::open(workspace).map_err(|error| io_error("open transaction workspace", error))?;
        if !workspace
            .metadata()
            .map_err(|error| io_error("inspect transaction workspace", error))?
            .is_dir()
        {
            return model_error("transaction workspace is not a directory");
        }
        let directory = if root_name == "." {
            workspace
        } else {
            validate_relative(root_name)?;
            let fd = fs::openat2(
                &workspace,
                root_name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
            )
            .map_err(|error| io_error("open transaction root", error))?;
            File::from(fd)
        };
        ensure_supported_directory(&directory)?;
        let identity = identity_bytes(
            &directory
                .metadata()
                .map_err(|error| io_error("inspect transaction root", error))?,
        );
        Ok(NativeRoot {
            directory: Arc::new(directory),
            identity,
        })
    }

    pub(in crate::hashline) fn root_identity(root: &NativeRoot) -> &[u8] {
        &root.identity
    }

    pub(in crate::hashline) fn observe(
        root: &NativeRoot,
        model_path: &str,
    ) -> Result<Observed, FunctionCallError> {
        let path = resolve(root, model_path)?;
        observe_resolved(&path)
    }

    pub(in crate::hashline) fn ensure_missing(
        root: &NativeRoot,
        model_path: &str,
    ) -> Result<(), FunctionCallError> {
        let path = resolve(root, model_path)?;
        match open_entry(&path, OFlags::RDONLY) {
            Ok(_) => model_error(format!("Hashline destination {model_path} already exists")),
            Err(error) if error == rustix::io::Errno::NOENT => Ok(()),
            Err(error) => Err(io_error("inspect transaction destination", error)),
        }
    }

    pub(in crate::hashline) fn lock_paths(
        root: &NativeRoot,
        prepared: &[PreparedMutation],
    ) -> Result<TransactionLease, FunctionCallError> {
        let mut parents = BTreeMap::<DirectoryIdentity, Arc<File>>::new();
        for model_path in mutation_paths(prepared) {
            let path = resolve(root, model_path)?;
            parents
                .entry(path.parent_identity)
                .or_insert_with(|| Arc::clone(&path.parent));
        }
        let covered_parents = parents.keys().copied().collect();
        let mut locks = Vec::with_capacity(parents.len());
        for parent in parents.into_values() {
            let lock = File::from(
                fs::openat(
                    &parent,
                    ".",
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|error| io_error("open transaction parent lease", error))?,
            );
            fs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
                FunctionCallError::RespondToModel(format!(
                    "transaction conflict: another commit owns a participating directory: {error}"
                ))
            })?;
            locks.push(lock);
        }
        for model_path in mutation_paths(prepared) {
            verify_path_parent(&resolve(root, model_path)?)?;
        }
        Ok(TransactionLease {
            root_identity: root.identity.clone(),
            covered_parents,
            _locks: locks,
        })
    }

    pub(in crate::hashline) fn write_journal(
        root: &NativeRoot,
        transaction_id: &str,
        prepared: &[PreparedMutation],
    ) -> Result<JournalHandle, FunctionCallError> {
        let directory = open_journal_directory(root, true)?.ok_or_else(|| {
            FunctionCallError::RespondToModel("transaction storage was not created".to_owned())
        })?;
        prune_terminal_receipts(root, &directory)?;
        let reservation =
            reserve_transaction(&directory, transaction_id, &exact_digest(&root.identity))?;
        let name = format!("{transaction_id}.json");
        let mutations = prepared
            .iter()
            .enumerate()
            .map(|(index, mutation)| {
                journal_mutation(
                    &exact_digest(&root.identity),
                    transaction_id,
                    index,
                    mutation,
                )
            })
            .collect();
        let mut journal = Journal {
            version: JOURNAL_VERSION,
            transaction_id: transaction_id.to_owned(),
            root_identity: exact_digest(&root.identity),
            state: JournalState::Preparing,
            progress: 0,
            rollback_progress: 0,
            rollback_limit: 0,
            outcome: None,
            mutations,
        };
        let _preparing_file = publish_journal(&directory, &name, &journal, "preparing")?;
        for (index, mutation) in prepared.iter().enumerate() {
            write_mutation_artifacts(&directory, &journal.mutations[index], mutation)?;
        }
        directory
            .sync_all()
            .map_err(|error| io_error("sync prepared transaction artifacts", error))?;
        fault_point("after-artifacts-dir-sync");
        journal.state = JournalState::Prepared;
        let file = publish_journal(&directory, &name, &journal, "prepared")?;
        Ok(JournalHandle {
            directory,
            file,
            _reservation: reservation,
            name,
            journal,
        })
    }

    pub(in crate::hashline) fn apply_prepared(
        root: &NativeRoot,
        lease: &TransactionLease,
        prepared: &[PreparedMutation],
        journal: &mut JournalHandle,
    ) -> Result<(), FunctionCallError> {
        require_lease(root, lease)?;
        persist_state(journal, JournalState::Committing, "committing")?;
        for (index, mutation) in prepared.iter().enumerate() {
            if let Err(forward_error) = apply_one(root, lease, mutation, index) {
                return match rollback_transaction(root, lease, prepared, index + 1, journal) {
                    Ok(()) => model_error(format!(
                        "transaction apply failed and immediate rollback restored all before-images: {forward_error}"
                    )),
                    Err(rollback_error) => model_error(format!(
                        "transaction apply failed: {forward_error}; rollback requires recovery: {rollback_error}"
                    )),
                };
            }
            fault_point(&format!("after-mutation-{index}"));
            journal.journal.progress = index + 1;
            persist_journal(journal, &format!("commit-progress-{index}"))?;
        }
        persist_state(journal, JournalState::Committed, "committed")
    }

    pub(in crate::hashline) fn remove_journal(
        _root: &NativeRoot,
        mut journal: JournalHandle,
    ) -> Result<(), FunctionCallError> {
        journal.journal.outcome = Some(TerminalOutcome::Committed);
        persist_state(&mut journal, JournalState::Cleaning, "cleaning-committed")?;
        cleanup_artifacts(&journal.directory, &journal.journal)?;
        persist_state(&mut journal, JournalState::Complete, "complete-committed")
    }

    pub(in crate::hashline) fn recover_pending(root: &NativeRoot) -> Result<(), FunctionCallError> {
        let Some(directory) = open_journal_directory(root, false)? else {
            return Ok(());
        };
        let mut names = directory_entries(&directory)?;
        names.sort();
        if names.len() > MAX_MUTATIONS.saturating_mul(8) {
            return model_error("transaction recovery storage exceeds its bounded artifact limit");
        }
        let mut first_error = None;
        for name in &names {
            if (has_extension(name, "next") || has_extension(name, "pending"))
                && let Err(error) = cleanup_unpublished(&directory, name)
            {
                first_error.get_or_insert(error);
            }
        }
        for name in &names {
            if !has_extension(name, "json") {
                continue;
            }
            if let Err(error) = recover_entry(root, &directory, name) {
                first_error.get_or_insert(error);
            }
        }
        for name in &names {
            let Some(transaction_id) = name.strip_suffix(".reserve") else {
                continue;
            };
            if names
                .iter()
                .any(|candidate| candidate == &format!("{transaction_id}.json"))
            {
                continue;
            }
            if let Err(error) = cleanup_orphan_reservation(root, &directory, name) {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn resolve(root: &NativeRoot, model_path: &str) -> Result<NativePath, FunctionCallError> {
        validate_relative(model_path)?;
        if model_path == STATE_DIRECTORY
            || model_path.starts_with(&format!("{STATE_DIRECTORY}/{TRANSACTION_DIRECTORY}"))
        {
            return model_error(
                "Hashline transaction paths cannot target internal recovery storage",
            );
        }
        let path = Path::new(model_path);
        let name = path.file_name().ok_or_else(|| {
            FunctionCallError::RespondToModel("transaction path has no file name".to_owned())
        })?;
        let parent_relative = path.parent().unwrap_or_else(|| Path::new("."));
        let parent = open_parent(&root.directory, parent_relative)?;
        ensure_supported_directory(&parent)?;
        let parent_identity = directory_identity(&parent)?;
        Ok(NativePath {
            root: Arc::clone(&root.directory),
            parent: Arc::new(parent),
            parent_relative: parent_relative.to_path_buf(),
            parent_identity,
            name: name.to_os_string(),
            model_path: model_path.to_owned(),
        })
    }

    fn validate_relative(model_path: &str) -> Result<(), FunctionCallError> {
        let path = Path::new(model_path);
        if model_path.is_empty() || path.is_absolute() {
            return model_error("Hashline paths must be non-empty and workspace-relative");
        }
        if path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return model_error(format!(
                "Hashline path {model_path} contains an invalid component"
            ));
        }
        Ok(())
    }

    fn open_parent(root: &File, relative: &Path) -> Result<File, FunctionCallError> {
        let relative = if relative == Path::new("") {
            Path::new(".")
        } else {
            relative
        };
        let fd = fs::openat2(
            root,
            relative,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|error| io_error("resolve transaction parent", error))?;
        Ok(File::from(fd))
    }

    fn open_entry(path: &NativePath, access: OFlags) -> Result<File, rustix::io::Errno> {
        fs::openat2(
            &path.parent,
            &path.name,
            access | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map(File::from)
    }

    fn open_named(directory: &File, name: &str, access: OFlags) -> Result<File, FunctionCallError> {
        fs::openat2(
            directory,
            name,
            access | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map(File::from)
        .map_err(|error| io_error("open transaction artifact", error))
    }

    fn verify_path_parent(path: &NativePath) -> Result<(), FunctionCallError> {
        let reopened = open_parent(&path.root, &path.parent_relative)?;
        if directory_identity(&reopened)? != path.parent_identity {
            return model_error(format!(
                "transaction path {} changed parent identity while leased",
                path.model_path
            ));
        }
        Ok(())
    }

    fn observe_resolved(path: &NativePath) -> Result<Observed, FunctionCallError> {
        verify_path_parent(path)?;
        let mut file = open_entry(path, OFlags::RDONLY)
            .map_err(|error| io_error("open transaction file", error))?;
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect transaction file", error))?;
        if !metadata.is_file() {
            return model_error(format!(
                "Hashline path {} is not a regular file",
                path.model_path
            ));
        }
        if metadata.nlink() != 1 {
            return model_error(format!(
                "Hashline path {} has multiple hard links",
                path.model_path
            ));
        }
        if metadata.len() > super::super::MAX_FILE_BYTES {
            return model_error("Hashline file exceeds the configured byte limit");
        }
        let mut bytes = Vec::with_capacity(bounded_capacity(metadata.len())?);
        file.read_to_end(&mut bytes)
            .map_err(|error| io_error("read transaction file", error))?;
        let after = file
            .metadata()
            .map_err(|error| io_error("reinspect transaction file", error))?;
        if !same_observation(&metadata, &after) {
            return model_error(format!(
                "Hashline path {} changed while it was observed; reread and retry",
                path.model_path
            ));
        }
        if bytes.contains(&0) {
            return model_error(format!(
                "Hashline path {} contains NUL/binary content",
                path.model_path
            ));
        }
        let text = String::from_utf8(bytes.clone()).map_err(|_| {
            FunctionCallError::RespondToModel(format!(
                "Hashline path {} is not valid UTF-8",
                path.model_path
            ))
        })?;
        Ok(Observed {
            bytes,
            text,
            metadata: FileMetadata::from_std(&metadata),
        })
    }

    fn require_lease(root: &NativeRoot, lease: &TransactionLease) -> Result<(), FunctionCallError> {
        if lease.root_identity != root.identity {
            return model_error("transaction lease belongs to a different root");
        }
        Ok(())
    }

    fn require_path_lease(
        lease: &TransactionLease,
        path: &NativePath,
    ) -> Result<(), FunctionCallError> {
        if !lease.covered_parents.contains(&path.parent_identity) {
            return model_error(format!(
                "transaction path {} is not covered by the retained lease",
                path.model_path
            ));
        }
        verify_path_parent(path)
    }

    fn apply_one(
        root: &NativeRoot,
        lease: &TransactionLease,
        mutation: &PreparedMutation,
        index: usize,
    ) -> Result<(), FunctionCallError> {
        match mutation {
            PreparedMutation::Write {
                path,
                before,
                after,
                metadata,
            } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                match before {
                    Some(expected) => require_bytes(&target, expected)?,
                    None => require_absent(&target)?,
                }
                atomic_write(
                    &target,
                    after,
                    &format!("mutation-{index}"),
                    metadata.as_ref(),
                )
            }
            PreparedMutation::Delete { path, before, .. } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                require_bytes(&target, before)?;
                fs::unlinkat(&target.parent, &target.name, AtFlags::empty())
                    .map_err(|error| io_error("delete transaction file", error))?;
                fault_point(&format!("mutation-{index}-after-unlink"));
                target
                    .parent
                    .sync_all()
                    .map_err(|error| io_error("sync transaction parent", error))?;
                fault_point(&format!("mutation-{index}-after-parent-sync"));
                Ok(())
            }
            PreparedMutation::Move {
                source,
                destination,
                before,
                after,
                metadata,
            } => {
                let source_path = resolve(root, source)?;
                let destination_path = resolve(root, destination)?;
                require_path_lease(lease, &source_path)?;
                require_path_lease(lease, &destination_path)?;
                require_bytes(&source_path, before)?;
                require_absent(&destination_path)?;
                atomic_write(
                    &destination_path,
                    after,
                    &format!("mutation-{index}-destination"),
                    Some(metadata),
                )?;
                fs::unlinkat(&source_path.parent, &source_path.name, AtFlags::empty())
                    .map_err(|error| io_error("remove transaction move source", error))?;
                fault_point(&format!("mutation-{index}-after-source-unlink"));
                source_path
                    .parent
                    .sync_all()
                    .map_err(|error| io_error("sync transaction move source parent", error))?;
                fault_point(&format!("mutation-{index}-after-source-parent-sync"));
                Ok(())
            }
        }
    }

    fn recover_one(
        root: &NativeRoot,
        lease: &TransactionLease,
        mutation: &PreparedMutation,
    ) -> Result<(), FunctionCallError> {
        match mutation {
            PreparedMutation::Write {
                path,
                before: Some(before),
                after,
                metadata,
            } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                match entry_evidence(&target)? {
                    Some(current) if evidence_matches(&current, before, metadata.as_ref()) => {
                        Ok(())
                    }
                    Some(current) if evidence_matches(&current, after, metadata.as_ref()) => {
                        atomic_write(&target, before, "recover-write", metadata.as_ref())
                    }
                    _ => recovery_conflict(path),
                }
            }
            PreparedMutation::Write {
                path,
                before: None,
                after,
                ..
            } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                match entry_bytes(&target)? {
                    None => Ok(()),
                    Some(current) if current == *after => {
                        fs::unlinkat(&target.parent, &target.name, AtFlags::empty())
                            .map_err(|error| io_error("recover transaction create", error))?;
                        fault_point("recover-create-after-unlink");
                        target
                            .parent
                            .sync_all()
                            .map_err(|error| io_error("sync recovered create parent", error))?;
                        fault_point("recover-create-after-parent-sync");
                        Ok(())
                    }
                    _ => recovery_conflict(path),
                }
            }
            PreparedMutation::Delete {
                path,
                before,
                metadata,
            } => {
                let target = resolve(root, path)?;
                require_path_lease(lease, &target)?;
                match entry_evidence(&target)? {
                    None => atomic_write(&target, before, "recover-delete", Some(metadata)),
                    Some(current) if evidence_matches(&current, before, Some(metadata)) => Ok(()),
                    _ => recovery_conflict(path),
                }
            }
            PreparedMutation::Move {
                source,
                destination,
                before,
                after,
                metadata,
            } => recover_move(root, lease, source, destination, before, after, metadata),
        }
    }

    fn recover_move(
        root: &NativeRoot,
        lease: &TransactionLease,
        source: &str,
        destination: &str,
        before: &[u8],
        after: &[u8],
        metadata: &FileMetadata,
    ) -> Result<(), FunctionCallError> {
        let source_path = resolve(root, source)?;
        let destination_path = resolve(root, destination)?;
        require_path_lease(lease, &source_path)?;
        require_path_lease(lease, &destination_path)?;
        let source_evidence = entry_evidence(&source_path)?;
        let destination_evidence = entry_evidence(&destination_path)?;
        match (source_evidence, destination_evidence) {
            (Some(source), None) if evidence_matches(&source, before, Some(metadata)) => Ok(()),
            (None, Some(destination)) if evidence_matches(&destination, after, Some(metadata)) => {
                atomic_write(&source_path, before, "recover-move", Some(metadata))?;
                fs::unlinkat(
                    &destination_path.parent,
                    &destination_path.name,
                    AtFlags::empty(),
                )
                .map_err(|error| io_error("recover move destination", error))?;
                fault_point("recover-move-after-destination-unlink");
                destination_path
                    .parent
                    .sync_all()
                    .map_err(|error| io_error("sync recovered move destination", error))?;
                fault_point("recover-move-after-destination-parent-sync");
                Ok(())
            }
            (Some(source), Some(destination))
                if evidence_matches(&source, before, Some(metadata))
                    && evidence_matches(&destination, after, Some(metadata)) =>
            {
                fs::unlinkat(
                    &destination_path.parent,
                    &destination_path.name,
                    AtFlags::empty(),
                )
                .map_err(|error| io_error("recover partial move destination", error))?;
                fault_point("recover-move-after-destination-unlink");
                destination_path
                    .parent
                    .sync_all()
                    .map_err(|error| io_error("sync recovered partial move", error))?;
                fault_point("recover-move-after-destination-parent-sync");
                Ok(())
            }
            _ => recovery_conflict(source),
        }
    }

    fn atomic_write(
        target: &NativePath,
        contents: &[u8],
        transition: &str,
        metadata: Option<&FileMetadata>,
    ) -> Result<(), FunctionCallError> {
        verify_path_parent(target)?;
        let temporary = temporary_name(&target.model_path, transition);
        let fd = fs::openat(
            &target.parent,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| io_error("stage transaction file", error))?;
        let mut file = File::from(fd);
        if let Some(mode) = metadata.and_then(|metadata| metadata.mode) {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(mode))
                .map_err(|error| io_error("restore transaction permissions", error))?;
        } else if let Ok(existing) = open_entry(target, OFlags::RDONLY) {
            file.set_permissions(
                existing
                    .metadata()
                    .map_err(|error| io_error("inspect transaction permissions", error))?
                    .permissions(),
            )
            .map_err(|error| io_error("preserve transaction permissions", error))?;
        }
        let result = (|| {
            file.write_all(contents)
                .map_err(|error| io_error("write transaction stage", error))?;
            file.sync_all()
                .map_err(|error| io_error("sync transaction stage", error))?;
            fault_point(&format!("{transition}-after-stage-sync"));
            fs::renameat(
                &target.parent,
                temporary.as_str(),
                &target.parent,
                &target.name,
            )
            .map_err(|error| io_error("publish transaction file", error))?;
            fault_point(&format!("{transition}-after-rename"));
            target
                .parent
                .sync_all()
                .map_err(|error| io_error("sync transaction parent", error))?;
            fault_point(&format!("{transition}-after-parent-sync"));
            require_bytes(target, contents)
        })();
        if result.is_err() {
            let _ = fs::unlinkat(&target.parent, temporary.as_str(), AtFlags::empty());
        }
        result
    }

    fn cleanup_temporary_files(
        root: &NativeRoot,
        lease: &TransactionLease,
        prepared: &[PreparedMutation],
    ) -> Result<(), FunctionCallError> {
        let mut candidates = Vec::new();
        for (index, mutation) in prepared.iter().enumerate() {
            match mutation {
                PreparedMutation::Write { path, before, .. } => {
                    candidates.push((path.as_str(), format!("mutation-{index}")));
                    if before.is_some() {
                        candidates.push((path.as_str(), "recover-write".to_owned()));
                    }
                }
                PreparedMutation::Delete { path, .. } => {
                    candidates.push((path.as_str(), "recover-delete".to_owned()));
                }
                PreparedMutation::Move {
                    source,
                    destination,
                    ..
                } => {
                    candidates.push((
                        destination.as_str(),
                        format!("mutation-{index}-destination"),
                    ));
                    candidates.push((source.as_str(), "recover-move".to_owned()));
                }
            }
        }
        for (model_path, transition) in candidates {
            let path = resolve(root, model_path)?;
            require_path_lease(lease, &path)?;
            let temporary = temporary_name(model_path, &transition);
            match fs::unlinkat(&path.parent, temporary.as_str(), AtFlags::empty()) {
                Ok(()) => path
                    .parent
                    .sync_all()
                    .map_err(|error| io_error("sync transaction temporary cleanup", error))?,
                Err(error) if error == rustix::io::Errno::NOENT => {}
                Err(error) => return Err(io_error("remove transaction temporary", error)),
            }
        }
        Ok(())
    }

    fn temporary_name(model_path: &str, transition: &str) -> String {
        format!(
            ".nanocodex-hashline-{}.tmp",
            exact_digest(format!("{model_path}:{transition}").as_bytes())
        )
    }

    fn require_bytes(path: &NativePath, expected: &[u8]) -> Result<(), FunctionCallError> {
        match entry_bytes(path)? {
            Some(bytes) if bytes == expected => Ok(()),
            _ => model_error(format!(
                "transaction evidence changed for {}; reread and rebuild the plan",
                path.model_path
            )),
        }
    }

    fn require_absent(path: &NativePath) -> Result<(), FunctionCallError> {
        if entry_bytes(path)?.is_none() {
            Ok(())
        } else {
            model_error(format!(
                "transaction destination {} is no longer absent",
                path.model_path
            ))
        }
    }

    fn entry_bytes(path: &NativePath) -> Result<Option<Vec<u8>>, FunctionCallError> {
        Ok(entry_evidence(path)?.map(|(bytes, _metadata)| bytes))
    }

    fn entry_evidence(
        path: &NativePath,
    ) -> Result<Option<(Vec<u8>, FileMetadata)>, FunctionCallError> {
        verify_path_parent(path)?;
        let mut file = match open_entry(path, OFlags::RDONLY) {
            Ok(file) => file,
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
            Err(error) => return Err(io_error("open transaction evidence", error)),
        };
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect transaction evidence", error))?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() > super::super::MAX_FILE_BYTES
        {
            return model_error(format!(
                "transaction evidence for {} is invalid",
                path.model_path
            ));
        }
        let file_metadata = FileMetadata::from_std(&metadata);
        let mut bytes = Vec::with_capacity(bounded_capacity(metadata.len())?);
        file.read_to_end(&mut bytes)
            .map_err(|error| io_error("read transaction evidence", error))?;
        Ok(Some((bytes, file_metadata)))
    }

    fn evidence_matches(
        current: &(Vec<u8>, FileMetadata),
        expected_bytes: &[u8],
        expected_metadata: Option<&FileMetadata>,
    ) -> bool {
        current.0 == expected_bytes
            && expected_metadata.is_none_or(|metadata| current.1 == *metadata)
    }

    fn mutation_paths(prepared: &[PreparedMutation]) -> impl Iterator<Item = &str> {
        prepared
            .iter()
            .flat_map(|mutation| match mutation {
                PreparedMutation::Write { path, .. } | PreparedMutation::Delete { path, .. } => {
                    [Some(path.as_str()), None]
                }
                PreparedMutation::Move {
                    source,
                    destination,
                    ..
                } => [Some(source.as_str()), Some(destination.as_str())],
            })
            .flatten()
    }

    fn journal_mutation(
        root_identity: &str,
        transaction_id: &str,
        index: usize,
        mutation: &PreparedMutation,
    ) -> JournalMutation {
        let evidence =
            |class: &str, bytes: &[u8], metadata: Option<FileMetadata>| ArtifactEvidence {
                name: format!("{root_identity}.{transaction_id}.{index}.{class}"),
                exact_digest: exact_digest(bytes),
                bytes: bytes.len(),
                metadata,
            };
        match mutation {
            PreparedMutation::Write {
                path,
                before,
                after,
                metadata,
            } => JournalMutation {
                kind: "write".to_owned(),
                path: path.clone(),
                destination: None,
                before: before
                    .as_deref()
                    .map(|bytes| evidence("backup", bytes, metadata.clone())),
                after: Some(evidence("staged", after, metadata.clone())),
            },
            PreparedMutation::Delete {
                path,
                before,
                metadata,
            } => JournalMutation {
                kind: "delete".to_owned(),
                path: path.clone(),
                destination: None,
                before: Some(evidence("backup", before, Some(metadata.clone()))),
                after: None,
            },
            PreparedMutation::Move {
                source,
                destination,
                before,
                after,
                metadata,
            } => JournalMutation {
                kind: "move".to_owned(),
                path: source.clone(),
                destination: Some(destination.clone()),
                before: Some(evidence("backup", before, Some(metadata.clone()))),
                after: Some(evidence("staged", after, Some(metadata.clone()))),
            },
        }
    }

    fn write_mutation_artifacts(
        directory: &File,
        journal: &JournalMutation,
        mutation: &PreparedMutation,
    ) -> Result<(), FunctionCallError> {
        match mutation {
            PreparedMutation::Write { before, after, .. } => {
                if let (Some(evidence), Some(bytes)) = (&journal.before, before.as_deref()) {
                    write_artifact(directory, evidence, bytes)?;
                }
                write_artifact(
                    directory,
                    journal.after.as_ref().ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "write journal lacks staged evidence".to_owned(),
                        )
                    })?,
                    after,
                )
            }
            PreparedMutation::Delete { before, .. } => write_artifact(
                directory,
                journal.before.as_ref().ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "delete journal lacks backup evidence".to_owned(),
                    )
                })?,
                before,
            ),
            PreparedMutation::Move { before, after, .. } => {
                write_artifact(
                    directory,
                    journal.before.as_ref().ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "move journal lacks backup evidence".to_owned(),
                        )
                    })?,
                    before,
                )?;
                write_artifact(
                    directory,
                    journal.after.as_ref().ok_or_else(|| {
                        FunctionCallError::RespondToModel(
                            "move journal lacks staged evidence".to_owned(),
                        )
                    })?,
                    after,
                )
            }
        }
    }

    fn write_artifact(
        directory: &File,
        evidence: &ArtifactEvidence,
        bytes: &[u8],
    ) -> Result<(), FunctionCallError> {
        if bytes.len() != evidence.bytes || exact_digest(bytes) != evidence.exact_digest {
            return model_error("transaction artifact evidence does not match prepared bytes");
        }
        let fd = fs::openat(
            directory,
            evidence.name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| io_error("reserve transaction artifact", error))?;
        let mut file = File::from(fd);
        file.write_all(bytes)
            .map_err(|error| io_error("write transaction artifact", error))?;
        file.sync_all()
            .map_err(|error| io_error("sync transaction artifact", error))
    }

    fn read_artifact(
        directory: &File,
        evidence: &ArtifactEvidence,
    ) -> Result<Vec<u8>, FunctionCallError> {
        if evidence.bytes > MAX_ARTIFACT_BYTES {
            return model_error("transaction artifact exceeds the per-file byte limit");
        }
        let mut file = open_named(directory, &evidence.name, OFlags::RDONLY)?;
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect transaction artifact", error))?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.len() != evidence.bytes as u64
            || metadata.mode() & 0o777 != 0o600
        {
            return model_error("transaction artifact metadata is invalid");
        }
        let mut bytes = Vec::with_capacity(evidence.bytes);
        file.read_to_end(&mut bytes)
            .map_err(|error| io_error("read transaction artifact", error))?;
        if exact_digest(&bytes) != evidence.exact_digest {
            return model_error("transaction artifact digest is invalid");
        }
        Ok(bytes)
    }

    fn prepared_from_journal(
        directory: &File,
        journal: &Journal,
    ) -> Result<Vec<PreparedMutation>, FunctionCallError> {
        journal
            .mutations
            .iter()
            .map(
                |mutation| match (mutation.kind.as_str(), mutation.before.as_ref()) {
                    ("write", before) => {
                        let before = before
                            .map(|evidence| read_artifact(directory, evidence))
                            .transpose()?;
                        let after_evidence = mutation.after.as_ref().ok_or_else(|| {
                            FunctionCallError::RespondToModel(
                                "write journal lacks staged evidence".to_owned(),
                            )
                        })?;
                        Ok(PreparedMutation::Write {
                            path: mutation.path.clone(),
                            before,
                            after: read_artifact(directory, after_evidence)?,
                            metadata: after_evidence.metadata.clone(),
                        })
                    }
                    ("delete", Some(before)) => Ok(PreparedMutation::Delete {
                        path: mutation.path.clone(),
                        before: read_artifact(directory, before)?,
                        metadata: before.metadata.clone().ok_or_else(|| {
                            FunctionCallError::RespondToModel(
                                "delete backup lacks metadata".to_owned(),
                            )
                        })?,
                    }),
                    ("move", Some(before)) => {
                        let after = mutation.after.as_ref().ok_or_else(|| {
                            FunctionCallError::RespondToModel(
                                "move journal lacks staged evidence".to_owned(),
                            )
                        })?;
                        Ok(PreparedMutation::Move {
                            source: mutation.path.clone(),
                            destination: mutation.destination.clone().ok_or_else(|| {
                                FunctionCallError::RespondToModel(
                                    "move journal lacks destination".to_owned(),
                                )
                            })?,
                            before: read_artifact(directory, before)?,
                            after: read_artifact(directory, after)?,
                            metadata: before.metadata.clone().ok_or_else(|| {
                                FunctionCallError::RespondToModel(
                                    "move backup lacks metadata".to_owned(),
                                )
                            })?,
                        })
                    }
                    _ => model_error("transaction journal contains an invalid mutation"),
                },
            )
            .collect()
    }

    fn prune_terminal_receipts(
        root: &NativeRoot,
        directory: &File,
    ) -> Result<(), FunctionCallError> {
        let mut complete = Vec::new();
        for name in directory_entries(directory)? {
            if !has_extension(&name, "json") {
                continue;
            }
            let file = open_named(directory, &name, OFlags::RDWR)?;
            match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::WOULDBLOCK => continue,
                Err(error) => return Err(io_error("lock terminal transaction receipt", error)),
            }
            let journal = read_journal(&file)?;
            validate_journal(root, &name, &journal)?;
            if journal.state == JournalState::Complete {
                complete.push((name, journal.transaction_id, file));
            }
        }
        complete.sort_by(|left, right| left.0.cmp(&right.0));
        let remove_count = complete
            .len()
            .saturating_add(1)
            .saturating_sub(MAX_TERMINAL_RECEIPTS);
        for (name, transaction_id, _file) in complete.into_iter().take(remove_count) {
            let reservation_name = format!("{transaction_id}.reserve");
            let reservation = open_named(directory, &reservation_name, OFlags::RDWR)?;
            match fs::flock(&reservation, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => {}
                Err(error) if error == rustix::io::Errno::WOULDBLOCK => continue,
                Err(error) => return Err(io_error("lock terminal transaction reservation", error)),
            }
            fs::unlinkat(directory, &name, AtFlags::empty())
                .map_err(|error| io_error("remove terminal transaction receipt", error))?;
            fs::unlinkat(directory, &reservation_name, AtFlags::empty())
                .map_err(|error| io_error("remove terminal transaction reservation", error))?;
        }
        directory
            .sync_all()
            .map_err(|error| io_error("sync terminal transaction receipt cleanup", error))
    }

    fn cleanup_orphan_reservation(
        root: &NativeRoot,
        directory: &File,
        name: &str,
    ) -> Result<(), FunctionCallError> {
        let mut reservation = open_named(directory, name, OFlags::RDWR)?;
        match fs::flock(&reservation, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => return Ok(()),
            Err(error) => return Err(io_error("lock orphan transaction reservation", error)),
        }
        let mut identity = String::new();
        reservation
            .seek(SeekFrom::Start(0))
            .map_err(|error| io_error("rewind orphan transaction reservation", error))?;
        reservation
            .take(65)
            .read_to_string(&mut identity)
            .map_err(|error| io_error("read orphan transaction reservation", error))?;
        if identity != exact_digest(&root.identity) {
            return model_error("orphan transaction reservation belongs to a different root");
        }
        fs::unlinkat(directory, name, AtFlags::empty())
            .map_err(|error| io_error("remove orphan transaction reservation", error))?;
        directory
            .sync_all()
            .map_err(|error| io_error("sync orphan transaction reservation cleanup", error))
    }

    fn reserve_transaction(
        directory: &File,
        transaction_id: &str,
        root_identity: &str,
    ) -> Result<File, FunctionCallError> {
        let name = format!("{transaction_id}.reserve");
        let fd = fs::openat(
            directory,
            name.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                FunctionCallError::RespondToModel(format!(
                    "duplicate transaction ID {transaction_id} is already reserved"
                ))
            } else {
                io_error("reserve transaction ID", error)
            }
        })?;
        let mut reservation = File::from(fd);
        fs::flock(&reservation, FlockOperation::NonBlockingLockExclusive)
            .map_err(|error| io_error("lock transaction reservation", error))?;
        reservation
            .write_all(root_identity.as_bytes())
            .map_err(|error| io_error("bind transaction reservation", error))?;
        reservation
            .sync_all()
            .map_err(|error| io_error("sync transaction reservation", error))?;
        directory
            .sync_all()
            .map_err(|error| io_error("sync transaction reservation directory", error))?;
        Ok(reservation)
    }

    fn persist_journal(
        journal: &mut JournalHandle,
        transition: &str,
    ) -> Result<(), FunctionCallError> {
        journal.file = publish_journal(
            &journal.directory,
            &journal.name,
            &journal.journal,
            transition,
        )?;
        Ok(())
    }

    fn persist_state(
        journal: &mut JournalHandle,
        state: JournalState,
        transition: &str,
    ) -> Result<(), FunctionCallError> {
        journal.journal.state = state;
        persist_journal(journal, transition)
    }

    fn rollback_transaction(
        root: &NativeRoot,
        lease: &TransactionLease,
        prepared: &[PreparedMutation],
        rollback_limit: usize,
        journal: &mut JournalHandle,
    ) -> Result<(), FunctionCallError> {
        if rollback_limit == 0 || rollback_limit > prepared.len() {
            return model_error("transaction rollback limit is invalid");
        }
        if journal.journal.rollback_limit == 0 {
            journal.journal.rollback_limit = rollback_limit;
        } else if journal.journal.rollback_limit != rollback_limit {
            return model_error("transaction rollback limit changed during recovery");
        }
        persist_state(journal, JournalState::RollingBack, "rolling-back")?;
        let mut first_error = None;
        for (reverse_index, mutation) in prepared[..rollback_limit]
            .iter()
            .rev()
            .enumerate()
            .skip(journal.journal.rollback_progress)
        {
            match recover_one(root, lease, mutation) {
                Ok(()) if first_error.is_none() => {
                    journal.journal.rollback_progress = reverse_index + 1;
                    persist_journal(journal, &format!("rollback-progress-{reverse_index}"))?;
                }
                Ok(()) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if let Some(error) = first_error {
            journal.journal.state = JournalState::RecoveryRequired;
            persist_journal(journal, "recovery-required")?;
            return Err(error);
        }
        journal.journal.outcome = Some(TerminalOutcome::RolledBack);
        persist_state(journal, JournalState::RolledBack, "rolled-back")?;
        persist_state(journal, JournalState::Cleaning, "cleaning-rolled-back")?;
        cleanup_artifacts(&journal.directory, &journal.journal)?;
        persist_state(journal, JournalState::Complete, "complete-rolled-back")
    }

    fn cleanup_artifacts(directory: &File, journal: &Journal) -> Result<(), FunctionCallError> {
        for evidence in journal
            .mutations
            .iter()
            .flat_map(|mutation| [mutation.before.as_ref(), mutation.after.as_ref()])
            .flatten()
        {
            match read_artifact(directory, evidence) {
                Ok(_) => {
                    fs::unlinkat(directory, evidence.name.as_str(), AtFlags::empty())
                        .map_err(|error| io_error("remove transaction artifact", error))?;
                }
                Err(error) => {
                    let missing = fs::openat2(
                        directory,
                        evidence.name.as_str(),
                        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                        Mode::empty(),
                        ResolveFlags::BENEATH
                            | ResolveFlags::NO_SYMLINKS
                            | ResolveFlags::NO_MAGICLINKS,
                    )
                    .is_err_and(|open_error| open_error == rustix::io::Errno::NOENT);
                    if !missing {
                        return Err(error);
                    }
                }
            }
        }
        directory
            .sync_all()
            .map_err(|error| io_error("sync transaction artifact cleanup", error))
    }

    fn recover_entry(
        root: &NativeRoot,
        directory: &Arc<File>,
        name: &str,
    ) -> Result<(), FunctionCallError> {
        let file = open_named(directory, name, OFlags::RDWR)?;
        match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => return Ok(()),
            Err(error) => return Err(io_error("lock recovery journal", error)),
        }
        let journal = read_journal(&file)?;
        validate_journal(root, name, &journal)?;
        if journal.state == JournalState::Complete {
            return Ok(());
        }
        let reservation_name = format!("{}.reserve", journal.transaction_id);
        let reservation = open_named(directory, &reservation_name, OFlags::RDWR)?;
        match fs::flock(&reservation, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => return Ok(()),
            Err(error) => return Err(io_error("lock recovery reservation", error)),
        }
        let mut handle = JournalHandle {
            directory: Arc::clone(directory),
            file,
            _reservation: reservation,
            name: name.to_owned(),
            journal,
        };
        if handle.journal.state == JournalState::Preparing {
            handle.journal.outcome = Some(TerminalOutcome::RolledBack);
            persist_state(&mut handle, JournalState::Cleaning, "recover-preparing")?;
            cleanup_artifacts(directory, &handle.journal)?;
            return persist_state(&mut handle, JournalState::Complete, "complete-preparing");
        }
        if handle.journal.state == JournalState::Prepared {
            handle.journal.outcome = Some(TerminalOutcome::RolledBack);
            persist_state(&mut handle, JournalState::Cleaning, "recover-prepared")?;
            cleanup_artifacts(directory, &handle.journal)?;
            return persist_state(&mut handle, JournalState::Complete, "complete-prepared");
        }
        if handle.journal.state == JournalState::Committed {
            handle.journal.outcome = Some(TerminalOutcome::Committed);
            persist_state(&mut handle, JournalState::Cleaning, "recover-committed")?;
            cleanup_artifacts(directory, &handle.journal)?;
            return persist_state(
                &mut handle,
                JournalState::Complete,
                "complete-recovered-commit",
            );
        }
        if handle.journal.state == JournalState::RolledBack {
            handle.journal.outcome = Some(TerminalOutcome::RolledBack);
            persist_state(&mut handle, JournalState::Cleaning, "recover-rolled-back")?;
            cleanup_artifacts(directory, &handle.journal)?;
            return persist_state(
                &mut handle,
                JournalState::Complete,
                "complete-recovered-rollback",
            );
        }
        if handle.journal.state == JournalState::Cleaning {
            cleanup_artifacts(directory, &handle.journal)?;
            return persist_state(&mut handle, JournalState::Complete, "complete-cleaning");
        }
        let prepared = prepared_from_journal(directory, &handle.journal)?;
        let lease = lock_paths(root, &prepared)?;
        cleanup_temporary_files(root, &lease, &prepared)?;
        let rollback_limit = if handle.journal.rollback_limit == 0 {
            handle
                .journal
                .progress
                .saturating_add(1)
                .min(prepared.len())
        } else {
            handle.journal.rollback_limit
        };
        rollback_transaction(root, &lease, &prepared, rollback_limit, &mut handle)
    }

    fn publish_journal(
        directory: &File,
        name: &str,
        journal: &Journal,
        transition: &str,
    ) -> Result<File, FunctionCallError> {
        let temporary = format!("{}.{}.next", journal.transaction_id, transition);
        let _ = fs::unlinkat(directory, temporary.as_str(), AtFlags::empty());
        let fd = fs::openat(
            directory,
            temporary.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|error| io_error("create transaction journal", error))?;
        let mut file = File::from(fd);
        fs::flock(&file, FlockOperation::NonBlockingLockExclusive)
            .map_err(|error| io_error("lock transaction journal", error))?;
        let bytes = serde_json::to_vec(journal)
            .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return model_error("transaction journal exceeds its byte limit");
        }
        file.write_all(&bytes)
            .map_err(|error| io_error("write transaction journal", error))?;
        file.sync_all()
            .map_err(|error| io_error("sync transaction journal", error))?;
        fault_point(&format!("{transition}-journal-file-sync"));
        fs::renameat(directory, temporary.as_str(), directory, name)
            .map_err(|error| io_error("publish transaction journal", error))?;
        fault_point(&format!("{transition}-journal-publish"));
        directory
            .sync_all()
            .map_err(|error| io_error("sync transaction journal directory", error))?;
        fault_point(&format!("{transition}-journal-dir-sync"));
        file.seek(SeekFrom::Start(0))
            .map_err(|error| io_error("rewind transaction journal", error))?;
        Ok(file)
    }

    fn read_journal(file: &File) -> Result<Journal, FunctionCallError> {
        let mut reader = file
            .try_clone()
            .map_err(|error| io_error("clone transaction journal", error))?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| io_error("rewind transaction journal", error))?;
        let mut bytes = Vec::new();
        reader
            .take((MAX_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| io_error("read transaction journal", error))?;
        if bytes.len() > MAX_JOURNAL_BYTES {
            return model_error("transaction journal exceeds its byte limit");
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            FunctionCallError::RespondToModel(format!("invalid transaction journal: {error}"))
        })
    }

    fn has_extension(name: &str, expected: &str) -> bool {
        Path::new(name)
            .extension()
            .is_some_and(|extension| extension == OsStr::new(expected))
    }

    fn validate_journal(
        root: &NativeRoot,
        name: &str,
        journal: &Journal,
    ) -> Result<(), FunctionCallError> {
        if journal.version != JOURNAL_VERSION
            || name != format!("{}.json", journal.transaction_id)
            || journal.root_identity != exact_digest(&root.identity)
            || journal.mutations.is_empty()
            || journal.mutations.len() > MAX_MUTATIONS
            || journal.progress > journal.mutations.len()
            || journal.rollback_limit > journal.mutations.len()
            || journal.rollback_progress > journal.rollback_limit
        {
            return model_error("transaction journal identity or bounds are invalid");
        }
        let artifact_prefix = format!("{}.{}.", journal.root_identity, journal.transaction_id);
        if journal
            .mutations
            .iter()
            .flat_map(|mutation| [mutation.before.as_ref(), mutation.after.as_ref()])
            .flatten()
            .any(|evidence| !evidence.name.starts_with(&artifact_prefix))
        {
            return model_error("transaction artifact identity is invalid");
        }
        Ok(())
    }

    fn open_journal_directory(
        root: &NativeRoot,
        create: bool,
    ) -> Result<Option<Arc<File>>, FunctionCallError> {
        let state = open_or_create_directory(&root.directory, STATE_DIRECTORY, create)?;
        let Some(state) = state else { return Ok(None) };
        let transactions = open_or_create_directory(&state, TRANSACTION_DIRECTORY, create)?;
        Ok(transactions.map(Arc::new))
    }

    fn open_or_create_directory(
        parent: &File,
        name: &str,
        create: bool,
    ) -> Result<Option<File>, FunctionCallError> {
        match fs::openat2(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        ) {
            Ok(fd) => {
                let file = File::from(fd);
                ensure_supported_directory(&file)?;
                Ok(Some(file))
            }
            Err(error) if error == rustix::io::Errno::NOENT && create => {
                fs::mkdirat(parent, name, Mode::from_raw_mode(0o700))
                    .map_err(|error| io_error("create transaction storage directory", error))?;
                parent
                    .sync_all()
                    .map_err(|error| io_error("sync transaction storage parent", error))?;
                open_or_create_directory(parent, name, false)
            }
            Err(error) if error == rustix::io::Errno::NOENT => Ok(None),
            Err(error) => Err(io_error("open transaction storage directory", error)),
        }
    }

    fn directory_entries(directory: &File) -> Result<Vec<String>, FunctionCallError> {
        let stream = Dir::read_from(directory)
            .map_err(|error| io_error("scan transaction storage", error))?;
        let mut names = Vec::new();
        for entry in stream {
            let entry = entry.map_err(|error| io_error("scan transaction storage", error))?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            names.push(String::from_utf8(name.to_bytes().to_vec()).map_err(|_| {
                FunctionCallError::RespondToModel(
                    "transaction storage contains a non-UTF-8 artifact".to_owned(),
                )
            })?);
        }
        Ok(names)
    }

    fn cleanup_unpublished(directory: &File, name: &str) -> Result<(), FunctionCallError> {
        let file = open_named(directory, name, OFlags::RDWR)?;
        match fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {
                fs::unlinkat(directory, name, AtFlags::empty())
                    .map_err(|error| io_error("remove unpublished transaction artifact", error))?;
                directory
                    .sync_all()
                    .map_err(|error| io_error("sync transaction storage cleanup", error))
            }
            Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(()),
            Err(error) => Err(io_error("lock unpublished transaction artifact", error)),
        }
    }

    fn ensure_supported_directory(directory: &File) -> Result<(), FunctionCallError> {
        let filesystem = fs::fstatfs(directory)
            .map_err(|error| io_error("inspect transaction filesystem", error))?;
        let filesystem_type = filesystem.f_type as i64;
        if filesystem_type == TMPFS_MAGIC {
            return Ok(());
        }
        if filesystem_type != EXT_SUPER_MAGIC {
            return model_error(format!(
                "unsupported: durable Hashline transactions require proven Linux ext-family or tmpfs semantics; found {filesystem_type:#x}"
            ));
        }
        let flags = fs::ioctl_getflags(directory)
            .map_err(|error| io_error("inspect transaction directory flags", error))?;
        if flags.intersects(IFlags::from_bits_retain(CASEFOLD_FLAG)) {
            return model_error(
                "unsupported: durable Hashline transactions require case-sensitive directory lookup",
            );
        }
        Ok(())
    }

    fn directory_identity(file: &File) -> Result<DirectoryIdentity, FunctionCallError> {
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect transaction directory identity", error))?;
        Ok(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn identity_bytes(metadata: &std::fs::Metadata) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&metadata.dev().to_le_bytes());
        bytes.extend_from_slice(&metadata.ino().to_le_bytes());
        bytes
    }

    fn same_observation(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.mode() == after.mode()
            && before.nlink() == after.nlink()
            && before.uid() == after.uid()
            && before.gid() == after.gid()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }

    fn bounded_capacity(length: u64) -> Result<usize, FunctionCallError> {
        usize::try_from(length).map_err(|_| {
            FunctionCallError::RespondToModel(
                "transaction file length does not fit this platform".to_owned(),
            )
        })
    }

    fn recovery_conflict<T>(path: &str) -> Result<T, FunctionCallError> {
        model_error(format!(
            "transaction recovery evidence for {path} is neither the retained before nor after state; manual recovery is required"
        ))
    }

    fn io_error(operation: &str, error: impl std::fmt::Display) -> FunctionCallError {
        FunctionCallError::RespondToModel(format!("{operation}: {error}"))
    }

    #[cfg(test)]
    fn fault_point(name: &str) {
        if std::env::var("NANOCODEX_HASHLINE_FAULT").as_deref() == Ok(name) {
            std::process::abort();
        }
    }

    #[cfg(not(test))]
    fn fault_point(_name: &str) {}
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::path::Path;

    use super::super::{FunctionCallError, Observed, PreparedMutation, model_error};

    #[derive(Debug)]
    pub(in crate::hashline) struct NativeRoot;
    #[derive(Debug)]
    pub(in crate::hashline) struct TransactionLease;
    #[derive(Debug)]
    pub(in crate::hashline) struct JournalHandle;

    pub(in crate::hashline) fn open_root(
        _workspace: &Path,
        _root_name: &str,
    ) -> Result<NativeRoot, FunctionCallError> {
        model_error(
            "unsupported: durable Hashline transactions currently require Linux ext-family or tmpfs filesystem semantics",
        )
    }

    pub(in crate::hashline) fn root_identity(_root: &NativeRoot) -> &[u8] {
        &[]
    }
    pub(in crate::hashline) fn observe(
        _root: &NativeRoot,
        _path: &str,
    ) -> Result<Observed, FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn ensure_missing(
        _root: &NativeRoot,
        _path: &str,
    ) -> Result<(), FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn recover_pending(
        _root: &NativeRoot,
    ) -> Result<(), FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn lock_paths(
        _root: &NativeRoot,
        _prepared: &[PreparedMutation],
    ) -> Result<TransactionLease, FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn write_journal(
        _root: &NativeRoot,
        _id: &str,
        _prepared: &[PreparedMutation],
    ) -> Result<JournalHandle, FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn apply_prepared(
        _root: &NativeRoot,
        _lease: &TransactionLease,
        _prepared: &[PreparedMutation],
        _journal: &mut JournalHandle,
    ) -> Result<(), FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
    pub(in crate::hashline) fn remove_journal(
        _root: &NativeRoot,
        _journal: JournalHandle,
    ) -> Result<(), FunctionCallError> {
        model_error("unsupported: durable Hashline transactions require Linux")
    }
}

pub(super) use platform::*;
