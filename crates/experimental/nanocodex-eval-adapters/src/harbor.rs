use std::path::{Path, PathBuf};

use nanocodex_eval::{
    Task,
    import::{CasePlan, DatasetImporter, DatasetPlan, ImportError, SourceIdentity},
};

use crate::{safe_case_id, sha256_values};

/// Lossless importer for Harbor task packages, including Terminal-Bench,
/// Frontier-Bench, and StableBench datasets.
#[derive(Clone, Debug)]
pub struct HarborDataset {
    name: Box<str>,
    root: PathBuf,
    revision: Box<str>,
}

impl HarborDataset {
    /// Creates an importer for one Harbor task or suite directory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        root: impl Into<PathBuf>,
        revision: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into().into_boxed_str(),
            root: root.into(),
            revision: revision.into().into_boxed_str(),
        }
    }
}

impl DatasetImporter for HarborDataset {
    fn plan(&self) -> Result<DatasetPlan, ImportError> {
        let roots = discover_tasks(&self.root)?;
        if roots.is_empty() {
            return Err(ImportError::Invalid(format!(
                "no Harbor task.toml files found under {}",
                self.root.display()
            )));
        }
        let mut loaded = Vec::with_capacity(roots.len());
        for root in roots {
            let task = Task::load(&root)?;
            let id = safe_case_id(task.name());
            loaded.push((id, root, task.package_digest().to_owned()));
        }
        let source_digest = sha256_values(
            loaded
                .iter()
                .flat_map(|(id, _, digest)| [id.as_bytes(), digest.as_bytes()]),
        );
        let source = SourceIdentity::new("harbor", self.revision.as_ref(), source_digest)?;
        let mut plan = DatasetPlan::new(self.name.as_ref(), source)?;
        for (id, root, _) in loaded {
            plan = plan.case(CasePlan::existing(id, root)?);
        }
        Ok(plan)
    }
}

fn discover_tasks(root: &Path) -> Result<Vec<PathBuf>, ImportError> {
    if root.join("task.toml").is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut tasks = Vec::new();
    for entry in ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
    {
        let entry = entry.map_err(|error| {
            ImportError::Invalid(format!("failed to walk {}: {error}", root.display()))
        })?;
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && entry.file_name() == "task.toml"
            && let Some(parent) = entry.path().parent()
        {
            tasks.push(parent.to_path_buf());
        }
    }
    tasks.sort();
    tasks.dedup();
    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use nanocodex_eval::import::ImportStore;

    use super::*;

    #[test]
    fn imports_an_existing_task_package_without_rewriting_it() {
        let task = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tasks/write-greeting");
        let store = tempfile::tempdir().unwrap();

        let imported = ImportStore::new(store.path())
            .import(&HarborDataset::new("fixture", task, "fixture@1"))
            .unwrap();

        assert_eq!(imported.tasks().len(), 1);
        assert_eq!(imported.tasks()[0].name(), "nanoeval/write-greeting");
    }
}
